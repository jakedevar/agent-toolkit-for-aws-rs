//! Tests for `toolkit startup terraform-policy`, migrated from
//! `skills/tf-best-practices/scripts/test_validate_terraform_policy.py`: the
//! shipped fixtures through the command, the regression cases for the rules,
//! and the unit cases of the HCL reader.

use std::ffi::OsString;
use std::fs;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use toolkit_core::Json;

use super::hcl::{attr_int, attr_string, extract_blocks, has_own_attr};
use super::{is_entire_internet, validate, TfFile};
use crate::run;
use crate::startup::artifacts::pyjson::loads;

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "toolkit-terraform-{label}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create temp dir");
        Self(path)
    }
}

impl Deref for TempDir {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn skill_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins/aws-startup-advisor/skills/tf-best-practices")
}

fn fixtures() -> PathBuf {
    skill_root().join("fixtures/terraform-policy")
}

fn chars(text: &str) -> Vec<char> {
    text.chars().collect()
}

fn exec(values: &[&str]) -> (u8, String, String) {
    let args: Vec<OsString> = values.iter().map(OsString::from).collect();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = run(&args, &mut out, &mut err);
    (
        code,
        String::from_utf8(out).expect("utf-8 stdout"),
        String::from_utf8(err).expect("utf-8 stderr"),
    )
}

/// (status, stdout + stderr) of the command on a directory.
fn policy(dir: &Path) -> (u8, String) {
    let (code, out, err) = exec(&[
        "startup",
        "terraform-policy",
        dir.to_str().expect("utf-8 path"),
    ]);
    (code, out + &err)
}

fn fixture(name: &str) -> (u8, String) {
    policy(&fixtures().join(name))
}

/// Writes one `vpc.tf` and returns the verdict.
fn verdict(body: &str) -> (u8, String) {
    let dir = TempDir::new("verdict");
    fs::write(dir.join("vpc.tf"), body).expect("write tf");
    policy(&dir)
}

/// The `--json` report of a directory.
fn report(dir: &Path) -> (u8, Json) {
    let out_dir = TempDir::new("report");
    let path = out_dir.join("verdict.json");
    let (code, _, _) = exec(&[
        "startup",
        "terraform-policy",
        dir.to_str().expect("utf-8 path"),
        "--json",
        path.to_str().expect("utf-8 path"),
    ]);
    let text = fs::read_to_string(&path).expect("verdict.json written");
    (code, loads(&text).expect("valid JSON report"))
}

fn rules(report: &Json) -> Vec<String> {
    let Some(Json::Array(violations)) = report.get("violations") else {
        panic!("no violations array");
    };
    violations
        .iter()
        .map(|v| {
            v.get("rule")
                .and_then(Json::as_str)
                .expect("rule")
                .to_string()
        })
        .collect()
}

fn summaries(report: &Json) -> String {
    let Some(Json::Array(violations)) = report.get("violations") else {
        panic!("no violations array");
    };
    violations
        .iter()
        .map(|v| v.get("summary").and_then(Json::as_str).expect("summary"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn assert_ok(result: &(u8, String)) {
    assert_eq!(result.0, 0, "{}", result.1);
    assert!(result.1.contains("POLICY_OK"), "{}", result.1);
}

fn assert_fails_with(result: &(u8, String), needle: &str) {
    assert_eq!(result.0, 1, "{}", result.1);
    assert!(result.1.contains(needle), "missing {needle}: {}", result.1);
}

#[test]
fn the_good_fixtures_pass_and_the_bad_ones_fail() {
    let mut names: Vec<String> = fs::read_dir(fixtures())
        .expect("read fixtures")
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    // Exact, not a floor: update deliberately when a fixture is added or removed.
    assert_eq!(names.len(), 31, "fixture count changed: {names:?}");
    for name in names {
        let result = fixture(&name);
        if name.starts_with("bad-") {
            assert_eq!(result.0, 1, "{name} must FAIL: {}", result.1);
            assert!(result.1.contains("POLICY_FAIL"), "{name}: {}", result.1);
        } else {
            assert_ok(&result);
        }
    }
}

#[test]
fn the_ok_line_lists_every_check() {
    let (code, out, err) = exec(&[
        "startup",
        "terraform-policy",
        fixtures()
            .join("good-https-redirect")
            .to_str()
            .expect("utf-8"),
    ]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(err, "");
    assert_eq!(
        out,
        "POLICY_OK | checks=alb_https,rds_not_public,db_sg_no_public_ingress,sg_no_public_admin_ingress,no_wildcard_iam,rds_encryption,elasticache_encryption,elasticache_cluster_encryption\n"
    );
}

#[test]
fn a_failing_run_prints_the_python_lines_on_stderr() {
    let (code, out, err) = exec(&[
        "startup",
        "terraform-policy",
        fixtures().join("bad-http-forward").to_str().expect("utf-8"),
    ]);
    assert_eq!(code, 1);
    assert_eq!(out, "");
    assert!(err.starts_with("POLICY_FAIL | checks=alb_https,"), "{err}");
    assert!(
        err.contains("\nPOLICY_FAIL | file=")
            && err.contains(" | rule=alb_http_redirect | reason=ALB HTTP listener '"),
        "{err}"
    );
    assert!(err.ends_with(" policy violation(s)\n"), "{err}");
}

#[test]
fn bad_fixtures_report_their_rules_in_the_json_verdict() {
    let (code, verdict) = report(&fixtures().join("bad-http-forward"));
    assert_eq!(code, 1);
    assert_eq!(
        verdict.get("policy_status").and_then(Json::as_str),
        Some("POLICY_FAIL")
    );
    assert!(rules(&verdict).contains(&"alb_http_redirect".to_string()));

    let (_, verdict) = report(&fixtures().join("bad-heroku-eb-elasticache"));
    let found = rules(&verdict);
    assert!(
        found.contains(&"elasticache_encryption_at_rest".to_string()),
        "{found:?}"
    );
    assert!(
        found.contains(&"alb_http_redirect".to_string()),
        "{found:?}"
    );

    let (_, verdict) = report(&fixtures().join("bad-rds-public-unencrypted"));
    let found = rules(&verdict);
    assert!(found.contains(&"rds_not_public".to_string()), "{found:?}");
    assert!(
        found.contains(&"rds_encryption_at_rest".to_string()),
        "{found:?}"
    );

    let (_, verdict) = report(&fixtures().join("bad-sg-public-ssh"));
    let text = summaries(&verdict);
    assert!(
        text.contains("22 (SSH)") && text.contains("6379 (Redis)"),
        "{text}"
    );
}

#[test]
fn the_json_report_has_the_python_shape() {
    let (code, verdict) = report(&fixtures().join("good-https-redirect"));
    assert_eq!(code, 0);
    assert_eq!(
        verdict,
        loads(r#"{"check": "policy", "policy_status": "POLICY_OK", "violations": []}"#)
            .expect("json")
    );
    let out_dir = TempDir::new("shape");
    let path = out_dir.join("v.json");
    let _ = exec(&[
        "startup",
        "terraform-policy",
        fixtures().join("bad-http-forward").to_str().expect("utf-8"),
        "--json",
        path.to_str().expect("utf-8"),
    ]);
    let text = fs::read_to_string(&path).expect("verdict");
    assert!(text.starts_with("{\n  \"check\": \"policy\",\n  \"policy_status\": \"POLICY_FAIL\",\n  \"violations\": [\n    {\n      \"check\": \"policy\",\n      \"rule\": \""), "{text}");
    assert!(text.contains("\"rule\": \"alb_http_redirect\""), "{text}");
    assert!(text.ends_with("}\n"), "{text}");
}

#[test]
fn a_missing_directory_is_a_usage_error_and_an_empty_one_a_violation() {
    let (code, out) = policy(Path::new("/nonexistent/terraform/dir"));
    assert_eq!(code, 2, "{out}");
    assert!(out.contains("not_a_directory"), "{out}");

    let dir = TempDir::new("empty");
    let result = policy(&dir);
    assert_fails_with(&result, "No .tf files");

    let (code, _, err) = exec(&["startup", "terraform-policy"]);
    assert_eq!(code, 2);
    assert!(err.contains("terraform_dir"), "{err}");
}

#[test]
fn an_unwritable_json_path_is_exit_two() {
    let dir = TempDir::new("jsonfail");
    fs::write(dir.join("a.tf"), "").expect("write tf");
    let bad = dir.join("no-such-dir/verdict.json");
    let (code, _, err) = exec(&[
        "startup",
        "terraform-policy",
        dir.to_str().expect("utf-8"),
        "--json",
        bad.to_str().expect("utf-8"),
    ]);
    assert_eq!(code, 2);
    assert!(
        err.starts_with("POLICY_FAIL | reason=json_write_failed | detail="),
        "{err}"
    );
}

#[test]
fn nested_and_sorted_files_are_read_with_relative_paths() {
    let dir = TempDir::new("nested");
    fs::create_dir_all(dir.join("modules/db")).expect("mkdir");
    fs::write(
        dir.join("modules/db/main.tf"),
        "resource \"aws_db_instance\" \"pg\" {\n  publicly_accessible = true\n  storage_encrypted = true\n}\n",
    )
    .expect("write tf");
    fs::write(dir.join("README.md"), "not terraform").expect("write md");
    let (code, out) = policy(&dir);
    assert_eq!(code, 1, "{out}");
    assert!(
        out.contains("file=modules/db/main.tf | line=1 | rule=rds_not_public"),
        "{out}"
    );
}

#[test]
fn a_linked_tf_file_is_refused() {
    let dir = TempDir::new("link");
    let real = dir.join("real.txt");
    fs::write(&real, "").expect("write target");
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&real, dir.join("linked.tf")).expect("symlink");
        let (code, out) = policy(&dir);
        assert_eq!(code, 1, "{out}");
        assert!(out.contains("symbolic links are not followed"), "{out}");
    }
}

#[test]
fn required_wildcard_cannot_hide_other_actions() {
    let result = verdict(
        "
resource \"aws_iam_role_policy\" \"mixed\" {
  role = \"example-role\"
  policy = jsonencode({
    Statement = [{
      Effect   = \"Allow\"
      Action   = [\"elasticbeanstalk:CreateStorageLocation\", \"s3:*\"]
      Resource = \"*\"
    }]
  })
}
",
    );
    assert_fails_with(&result, "no_wildcard_iam");
}

#[test]
fn the_isolated_storage_location_statement_is_allowed() {
    let result = verdict(
        "
resource \"aws_iam_role_policy\" \"eb\" {
  role = \"r\"
  policy = jsonencode({
    Statement = [
      {
        Effect   = \"Allow\"
        Action   = \"elasticbeanstalk:CreateStorageLocation\"
        Resource = \"*\"
      },
      {
        Effect   = \"Allow\"
        Action   = [\"s3:GetObject\"]
        Resource = [\"arn:aws:s3:::b/*\"]
      }
    ]
  })
}
",
    );
    assert_ok(&result);
}

#[test]
fn wildcard_forms_are_recognised() {
    for policy_body in [
        "\"Effect\": \"Allow\", \"Action\": \"*\", \"Resource\": \"arn:aws:s3:::b\"",
        "\"Effect\": \"Allow\", \"Action\": \"s3:Get*\", \"Resource\": [ \"*\" ]",
        "Effect = \"Allow\", NotAction = \"*\", Resource = \"x\"",
    ] {
        let body = format!(
            "resource \"aws_iam_policy\" \"p\" {{\n  policy = <<EOT\n{{{policy_body}}}\nEOT\n}}\n"
        );
        assert_fails_with(&verdict(&body), "no_wildcard_iam");
    }
    let scoped = "resource \"aws_iam_policy\" \"p\" {\n  policy = jsonencode({\n    Effect = \"Allow\"\n    Action = [\"s3:Get*\", \"*\"]\n    Resource = \"arn:aws:s3:::b\"\n  })\n}\n";
    assert_ok(&verdict(scoped));
    let deny = "resource \"aws_iam_policy\" \"p\" {\n  policy = jsonencode({\n    Effect = \"Deny\"\n    Action = \"*\"\n    Resource = \"*\"\n  })\n}\n";
    assert_ok(&verdict(deny));
}

#[test]
fn heroku_generate_phase_runs_the_policy_check_after_the_eks_fragment() {
    let phase_dir = skill_root().join("../heroku-to-aws/references/phases/generate");
    let phase = fs::read_to_string(phase_dir.join("generate.md")).expect("generate.md");
    let fragment = fs::read_to_string(phase_dir.join("generate-terraform.md")).expect("fragment");
    let assembler = fs::read_to_string(phase_dir.join("generate-assemble.md")).expect("assembler");
    assert!(
        phase.find("_id: eks-generate").expect("eks-generate")
            < phase.find("_assemble:").expect("_assemble")
    );
    assert!(fragment.contains("Defer the authoritative Terraform policy check to the assembler"));
    assert!(
        assembler.contains("Authoritative Terraform policy check (after all Terraform producers)")
    );
    assert!(assembler.contains("after `eks-generate`"));
}

fn sg_fixture(attrs: &str, port: u32) -> String {
    format!(
        "
resource \"aws_security_group\" \"under_test\" {{
  name   = \"sg\"
  vpc_id = \"vpc-123\"

  ingress {{
    from_port = {port}
    to_port   = {port}
    protocol  = \"tcp\"
    {attrs}
  }}
}}
"
    )
}

#[test]
fn ipv6_public_ingress_is_reported_with_its_own_range() {
    let (code, verdict) = report(&fixtures().join("bad-sg-public-ipv6"));
    assert_eq!(code, 1);
    let found = rules(&verdict);
    assert!(
        found.contains(&"sg_no_public_admin_ingress".to_string()),
        "{found:?}"
    );
    assert!(
        found.contains(&"db_sg_no_public_ingress".to_string()),
        "{found:?}"
    );
    let text = summaries(&verdict);
    assert!(text.contains("::/0") && text.contains("22 (SSH)"), "{text}");
    assert!(!text.contains("0.0.0.0/0"), "{text}");
}

#[test]
fn ipv4_public_ingress_is_caught_whichever_list_comes_first() {
    let ipv6_first =
        "ipv6_cidr_blocks = [\"2001:db8:1234::/48\"]\n    cidr_blocks      = [\"0.0.0.0/0\"]";
    let ipv4_first =
        "cidr_blocks      = [\"0.0.0.0/0\"]\n    ipv6_cidr_blocks = [\"2001:db8:1234::/48\"]";
    for attrs in [ipv6_first, ipv4_first] {
        let result = verdict(&sg_fixture(attrs, 22));
        assert_fails_with(&result, "22 (SSH)");
        assert!(result.1.contains("0.0.0.0/0"), "{}", result.1);
    }
}

#[test]
fn an_attribute_sharing_the_opening_brace_line_is_read() {
    let body = "
resource \"aws_security_group\" \"brace_same_line\" {
  name   = \"sg\"
  vpc_id = \"vpc-123\"

  ingress { cidr_blocks = [\"0.0.0.0/0\"]
    from_port = 22
    to_port   = 22
    protocol  = \"tcp\"
  }
}
";
    assert_fails_with(&verdict(body), "22 (SSH)");
}

#[test]
fn every_spelling_of_the_ipv6_zero_prefix_fires() {
    for spelling in [
        "::/0",
        "::0/0",
        "0:0:0:0:0:0:0:0/0",
        "0000:0000:0000:0000:0000:0000:0000:0000/0",
    ] {
        let result = verdict(&sg_fixture(
            &format!("ipv6_cidr_blocks = [\"{spelling}\"]"),
            22,
        ));
        assert_fails_with(&result, "sg_no_public_admin_ingress");
    }
}

#[test]
fn the_whole_internet_test_matches_ipaddress_semantics() {
    for yes in [
        "0.0.0.0/0",
        "1.2.3.4/0",
        "0.0.0.0/00",
        "0.0.0.0/0.0.0.0",
        "::/0",
        "::1/0",
        "2001:db8::/0",
    ] {
        assert!(is_entire_internet(yes), "{yes}");
    }
    for no in [
        "10.0.0.0/8",
        "0.0.0.0/1",
        "128.0.0.0/1",
        "2001:db8:1234::/48",
        "0.0.0.0",
        "::",
        "var.x",
        "${var.cidr}",
        "0.0.0.0/",
        "/0",
        "0.0.0.0/0/0",
        "0.0.0.0/255.255.255.255",
        "0.0.0.0/-0",
        "256.0.0.0/0",
        "",
    ] {
        assert!(!is_entire_internet(no), "{no}");
    }
}

#[test]
fn scoped_and_non_literal_cidrs_stay_fail_open() {
    for spelling in [
        "10.0.0.0/8",
        "2001:db8:1234::/48",
        "0.0.0.0/1",
        "128.0.0.0/1",
    ] {
        let attr = if spelling.contains(':') {
            "ipv6_cidr_blocks"
        } else {
            "cidr_blocks"
        };
        assert_ok(&verdict(&sg_fixture(
            &format!("{attr} = [\"{spelling}\"]"),
            22,
        )));
    }
    for inner in ["[var.admin_cidrs]", "[var.cidr]", "[local.allowed]"] {
        assert_ok(&verdict(&sg_fixture(&format!("cidr_blocks = {inner}"), 22)));
    }
}

#[test]
fn a_cidr_in_a_comment_is_not_an_allowed_range() {
    let scoped = "
    cidr_blocks = [
      # TODO: was 0.0.0.0/0
      \"10.0.0.0/8\",
    ]";
    assert_ok(&verdict(&sg_fixture(scoped, 3389)));
    let real = "
    cidr_blocks = [
      # temporary, remove before prod
      \"0.0.0.0/0\",
    ]";
    assert_fails_with(&verdict(&sg_fixture(real, 3389)), "3389 (RDP)");
}

#[test]
fn a_commented_out_cidr_attribute_is_not_read() {
    let body = sg_fixture(
        "# cidr_blocks = [\"0.0.0.0/0\"]\n    security_groups = [\"sg-abc123\"]",
        22,
    );
    assert_ok(&verdict(&body));
}

#[test]
fn a_comment_marker_inside_a_string_does_not_swallow_the_real_cidr() {
    let body = sg_fixture(
        "description = \"temp /* migration window\"\n    cidr_blocks = [\"0.0.0.0/0\"]\n    # closes */ in runbook",
        22,
    );
    assert_fails_with(&verdict(&body), "22 (SSH)");
}

#[test]
fn a_nested_forward_block_before_type_does_not_false_fail() {
    let body = "
resource \"aws_lb\" \"app\" {
  name     = \"app-alb\"
  internal = false
}

resource \"aws_lb_listener\" \"https\" {
  load_balancer_arn = aws_lb.app.arn
  port              = 443
  protocol          = \"HTTPS\"
  ssl_policy        = \"ELBSecurityPolicy-TLS13-1-2-2021-06\"
  certificate_arn   = var.acm_certificate_arn

  default_action {
    forward {
      target_group {
        arn = aws_lb_target_group.app.arn
      }
    }
    type = \"forward\"
  }
}

resource \"aws_lb_listener\" \"http_redirect\" {
  load_balancer_arn = aws_lb.app.arn
  port              = 80
  protocol          = \"HTTP\"

  default_action {
    type = \"redirect\"
    redirect {
      port        = \"443\"
      protocol    = \"HTTPS\"
      status_code = \"HTTP_301\"
    }
  }
}
";
    assert_ok(&verdict(body));
}

#[test]
fn a_missing_https_listener_names_the_load_balancer_file() {
    let dir = TempDir::new("nolistener");
    fs::write(dir.join("a.tf"), "# nothing\n").expect("write tf");
    fs::write(
        dir.join("b.tf"),
        "resource \"aws_lb\" \"app\" {\n  internal = false\n}\n",
    )
    .expect("write tf");
    let (code, out) = policy(&dir);
    assert_eq!(code, 1, "{out}");
    assert!(
        out.contains("file=b.tf | line=0 | rule=alb_https_listener | reason=Internet-facing ALB requires an HTTPS listener on port 443 with certificate_arn and a forward action"),
        "{out}"
    );
}

#[test]
fn quoted_ports_are_read_like_bare_ones() {
    let body = "
resource \"aws_security_group\" \"bad\" {
  name = \"bad-ssh-sg\"

  ingress {
    from_port   = \"22\"
    to_port     = \"22\"
    protocol    = \"tcp\"
    cidr_blocks = [\"0.0.0.0/0\"]
  }
}
";
    assert_fails_with(&verdict(body), "22 (SSH)");
}

#[test]
fn attr_int_reads_bare_and_quoted_integers() {
    assert_eq!(attr_int(&chars("  port = 443\n"), "port"), Some(443));
    assert_eq!(attr_int(&chars("  port = \"443\"\n"), "port"), Some(443));
    assert_eq!(
        attr_int(&chars("  from_port = \"0\"\n"), "from_port"),
        Some(0)
    );
    assert_eq!(attr_int(&chars("  protocol = \"HTTPS\"\n"), "port"), None);
}

#[test]
fn attr_int_rejects_non_literal_quoted_values() {
    for body in [
        "  port = \"443x\"\n",
        "  port = \"x443\"\n",
        "  port = \"443-444\"\n",
        "  port = \"${var.port}\"\n",
        "  port = \" 443\"\n",
        "  port = \"\"\n",
        "  port = var.port\n",
        "  port = local.ports[0]\n",
    ] {
        assert_eq!(attr_int(&chars(body), "port"), None, "{body}");
    }
}

const ATTRS_AFTER_NESTED_BLOCK: &str = "{
  load_balancer_arn = aws_lb.app.arn

  default_action {
    type = \"redirect\"
    redirect {
      port     = \"443\"
      protocol = \"HTTPS\"
    }
  }

  port     = 80
  protocol = \"HTTP\"
}";

#[test]
fn attributes_are_read_from_the_own_block_not_a_nested_one() {
    let block = chars(ATTRS_AFTER_NESTED_BLOCK);
    assert_eq!(attr_int(&block, "port"), Some(80));
    assert_eq!(attr_string(&block, "protocol").as_deref(), Some("HTTP"));
    let nested_only =
        chars("{\n  default_action {\n    redirect {\n      port = \"443\"\n    }\n  }\n}");
    assert_eq!(attr_int(&nested_only, "port"), None);
}

#[test]
fn interpolations_do_not_hide_later_attributes() {
    let body =
        chars("{\n  identifier        = \"${var.project}-db\"\n  storage_encrypted = true\n}");
    assert_eq!(
        attr_string(&body, "storage_encrypted").as_deref(),
        Some("true")
    );
}

#[test]
fn a_quote_nested_in_an_interpolation_does_not_end_the_string() {
    for body in [
        "{\n  identifier = \"${lookup(var.m, \"a{b\")}\"\n  port = 80\n}",
        "{\n  n = \"${replace(v, \"}\", \"\")}\"\n  port = 80\n}",
        "{\n  n = \"${replace(v, \"{}\", \"\")}\"\n  port = 80\n}",
        "{\n  a = \"${lookup(var.tags, \"Name\")}\"\n  port = 80\n}",
        "{\n  a = \"${f(\"x{\")}-${g(\"y}\")}\"\n  port = 80\n}",
    ] {
        assert_eq!(attr_int(&chars(body), "port"), Some(80), "{body}");
    }
}

#[test]
fn braces_in_comments_strings_and_heredocs_do_not_hide_attributes() {
    for body in [
        "{\n  # primary { instance\n  port = 80\n}",
        "{\n  // primary { instance\n  port = 80\n}",
        "{\n  /* primary { instance */\n  port = 80\n}",
        "{\n  identifier = \"primary { db\"\n  port = 80\n}",
        "{\n  name = \"a { b\"\n  port = 80\n}",
        "{\n  policy = <<EOT\n  { not code }\nEOT\n  port = 80\n}",
    ] {
        assert_eq!(attr_int(&chars(body), "port"), Some(80), "{body}");
    }
}

#[test]
fn extraction_is_not_truncated_by_a_closing_brace_that_is_not_a_delimiter() {
    for filler in [
        "  # capacity review } pending",
        "  // capacity review } pending",
        "  /* capacity review } pending */",
        "  identifier = \"primary } db\"",
        "  description = <<EOT\n  a closing } brace\nEOT",
        "  n = \"${replace(v, \"}\", \"\")}\"",
    ] {
        let content = chars(&format!(
            "resource \"aws_db_instance\" \"pg\" {{\n{filler}\n  publicly_accessible = true\n}}\n"
        ));
        let blocks = extract_blocks(&content, "aws_db_instance");
        assert_eq!(blocks.len(), 1, "{filler}");
        assert_eq!(
            attr_string(&blocks[0].body, "publicly_accessible").as_deref(),
            Some("true"),
            "{filler}"
        );
    }
}

#[test]
fn a_regular_heredoc_terminator_must_be_unindented() {
    let regular = chars("{\n  policy = <<EOT\n  EOT\n  port = 443\nEOT\n  port = 80\n}");
    assert_eq!(attr_int(&regular, "port"), Some(80));
    let indented = chars("{\n  policy = <<-EOT\n  { not code }\n  EOT\n  port = 80\n}");
    assert_eq!(attr_int(&indented, "port"), Some(80));
}

#[test]
fn the_presence_probe_agrees_with_the_value_reader() {
    let only_commented =
        chars("{\n  identifier = \"pg\"\n  /*\n  storage_encrypted = false\n  */\n}");
    assert!(!has_own_attr(&only_commented, "storage_encrypted"));
    assert_eq!(attr_string(&only_commented, "storage_encrypted"), None);
    let real = chars("{\n  storage_encrypted = var.encrypt\n}");
    assert!(has_own_attr(&real, "storage_encrypted"));
    assert_eq!(attr_string(&real, "storage_encrypted"), None);
}

#[test]
fn a_commented_out_resource_or_attribute_is_ignored() {
    let ghost =
        chars("# resource \"aws_db_instance\" \"ghost\" {\n#   publicly_accessible = true\n# }\n");
    assert!(extract_blocks(&ghost, "aws_db_instance").is_empty());
    assert_eq!(attr_int(&chars("{\n  # port = 443\n}"), "port"), None);
    assert_eq!(attr_int(&chars("{\n  // port = 443\n}"), "port"), None);
    assert_eq!(
        attr_int(&chars("{\n  /*\n  port = 443\n  */\n}"), "port"),
        None
    );
    assert_eq!(
        attr_string(
            &chars("{\n  /*\n  protocol = \"HTTPS\"\n  */\n}"),
            "protocol"
        ),
        None
    );
}

#[test]
fn string_and_bool_attributes_follow_the_patterns() {
    assert_eq!(
        attr_string(&chars("{\n  a = \"x y\"\n}"), "a").as_deref(),
        Some("x y")
    );
    assert_eq!(
        attr_string(
            &chars("{\n  Publicly_Accessible = TRUE\n}"),
            "publicly_accessible"
        )
        .as_deref(),
        Some("true")
    );
    assert_eq!(attr_string(&chars("{\n  a = truely\n}"), "a"), None);
    assert_eq!(
        attr_string(&chars("{\n  a = false # no\n}"), "a").as_deref(),
        Some("false")
    );
    assert_eq!(attr_string(&chars("{\n  a = \"unterminated\n}"), "a"), None);
}

#[test]
fn rds_and_elasticache_rules_fail_open_on_variables() {
    let rds = |attrs: &str| {
        format!("resource \"aws_db_instance\" \"pg\" {{\n  identifier = \"pg\"\n{attrs}\n}}\n")
    };
    assert_ok(&verdict(&rds("  storage_encrypted = true")));
    assert_ok(&verdict(&rds("  storage_encrypted = var.encrypt")));
    assert_fails_with(
        &verdict(&rds("  storage_encrypted = false")),
        "rds_encryption_at_rest",
    );
    assert_fails_with(&verdict(&rds("")), "rds_encryption_at_rest");
    assert_fails_with(
        &verdict(&rds(
            "  storage_encrypted = true\n  publicly_accessible = true",
        )),
        "rds_not_public",
    );
    let redis = |attrs: &str| {
        format!(
            "resource \"aws_elasticache_cluster\" \"c\" {{\n  engine = \"redis\"\n{attrs}\n}}\n"
        )
    };
    assert_fails_with(&verdict(&redis("")), "transit_encryption_enabled");
    assert_ok(&verdict(&redis("  transit_encryption_enabled = true")));
    assert_ok(&verdict(&redis("  replication_group_id = \"rg\"")));
}

#[test]
fn validate_reports_no_tf_files_as_one_violation() {
    let violations = validate(&[]);
    assert_eq!(violations.len(), 1);
    assert_eq!(violations[0].rule, "no_tf_files");
    assert_eq!(violations[0].file, ".");
    assert_eq!(violations[0].line, 0);
    let one = [TfFile {
        rel: "a.tf".to_string(),
        content: chars(""),
    }];
    assert!(validate(&one).is_empty());
}

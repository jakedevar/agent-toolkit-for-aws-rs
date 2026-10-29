//! `toolkit startup terraform-policy DIR [--json FILE]`: static policy checks
//! on generated Terraform, with no provider init (port of
//! `validate-terraform-policy.py` of the `tf-best-practices` skill).
//!
//! A read-only verdict producer: it never edits `.tf` files and never touches
//! run state. Each rule is fail-open on ambiguity and fires only on
//! unambiguous, in-block literal evidence, so a valid stack is never falsely
//! blocked. The rules are the ALB TLS posture, public RDS, public database and
//! admin ports in inline security-group ingress (IPv4 and IPv6), wildcard IAM
//! `Allow` statements, RDS and `ElastiCache` encryption. See
//! `docs/migration/startup-validators.md` for the parity table.

use std::fs;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::path::{Path, PathBuf};

use toolkit_core::{neutralize_line, Json, ToolkitError};

use self::hcl::{
    attr_int, attr_list_inner, attr_string, default_action_type, extract_blocks, has_own_attr,
    own_blocks, strip_comments,
};
use super::artifacts::pyjson::dumps;
use super::read_text;
use crate::manifests::resolve_lenient;
use crate::{Invocation, Output};

mod hcl;
mod iam;

/// Registration of the `startup-terraform` slice: its routes and help text (see `crate::registry`).
pub(crate) const SLICE: crate::registry::Slice = crate::registry::Slice {
    name: "startup-terraform",
    commands: &[crate::CommandSpec {
        path: &["startup", "terraform-policy"],
        flags: &[],
        value_flags: &["--json"],
        strict_values: false,
        repeatable_flags: &[],
        max_positionals: 1,
        passthrough: false,
        handler: crate::startup::terraform_policy_command,
    }],
    synopsis: r"  toolkit startup terraform-policy DIR [--json FILE]
",
    description: r"  startup terraform-policy
                Static policy checks on the generated Terraform in DIR: ALB TLS
                posture, public RDS, public database and admin ports in inline
                security-group ingress, wildcard IAM, RDS and ElastiCache
                encryption. --json FILE writes the verdict. Violations are
                POLICY_FAIL lines on stderr and exit status 1.
",
    exit_notes: "",
    doc: "docs/migration/startup-validators.md",
    synopsis_rank: 110,
    description_rank: 100,
};

#[cfg(test)]
mod tests;

/// The check names the summary line lists.
const CHECKS: &str = "alb_https,rds_not_public,db_sg_no_public_ingress,sg_no_public_admin_ingress,\
no_wildcard_iam,rds_encryption,elasticache_encryption,elasticache_cluster_encryption";

/// Database ports covered by `db_sg_no_public_ingress`.
const DB_PORTS: [i128; 2] = [5432, 3306];

/// Well-known admin and datastore ports that are never legitimately public,
/// with the label the report prints. 5432/3306 (the database rule) and the web
/// ports are deliberately absent.
const SENSITIVE_PORTS: [(i128, &str); 8] = [
    (22, "SSH"),
    (3389, "RDP"),
    (6379, "Redis"),
    (11211, "Memcached"),
    (27017, "MongoDB"),
    (9200, "Elasticsearch"),
    (9300, "Elasticsearch"),
    (5601, "Kibana"),
];

/// One `.tf` file: its path relative to the Terraform directory and content.
pub(crate) struct TfFile {
    pub(crate) rel: String,
    pub(crate) content: Vec<char>,
}

/// One policy violation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Violation {
    pub(crate) rule: &'static str,
    pub(crate) file: String,
    pub(crate) line: usize,
    pub(crate) summary: String,
    pub(crate) fix_hint: String,
}

impl Violation {
    fn new(
        rule: &'static str,
        file: &str,
        line: usize,
        summary: String,
        fix_hint: impl Into<String>,
    ) -> Self {
        Self {
            rule,
            file: file.to_string(),
            line,
            summary,
            fix_hint: fix_hint.into(),
        }
    }

    fn as_json(&self) -> Json {
        Json::object(vec![
            ("check", Json::string("policy")),
            ("rule", Json::string(self.rule)),
            ("file", Json::string(self.file.as_str())),
            (
                "line",
                Json::Int(i64::try_from(self.line).unwrap_or(i64::MAX)),
            ),
            ("severity", Json::string("error")),
            ("summary", Json::string(self.summary.as_str())),
            ("fix_hint", Json::string(self.fix_hint.as_str())),
        ])
    }
}

fn text(chars: &[char]) -> String {
    chars.iter().collect()
}

/// Every `*.tf` file below `dir`, sorted by path components. Links are not
/// followed and a linked file is an error.
fn read_tf_files(dir: &Path) -> Result<Vec<TfFile>, String> {
    // `*.tf` is a case-sensitive glob in the Python script.
    #[allow(clippy::case_sensitive_file_extension_comparisons)]
    fn walk(dir: &Path, prefix: &[String], found: &mut Vec<Vec<String>>) -> Result<(), String> {
        let entries =
            fs::read_dir(dir).map_err(|error| format!("cannot read {}: {error}", dir.display()))?;
        for entry in entries {
            let entry = entry.map_err(|error| format!("cannot read {}: {error}", dir.display()))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let kind = entry
                .file_type()
                .map_err(|error| format!("cannot read {}: {error}", entry.path().display()))?;
            let mut parts = prefix.to_vec();
            parts.push(name.clone());
            if kind.is_dir() {
                walk(&entry.path(), &parts, found)?;
            } else if name.ends_with(".tf") {
                found.push(parts);
            }
        }
        Ok(())
    }

    let mut found = Vec::new();
    walk(dir, &[], &mut found)?;
    found.sort();
    found
        .into_iter()
        .map(|parts| {
            let rel = parts.join("/");
            let content = read_text(&dir.join(&rel), &rel)?;
            Ok(TfFile {
                rel,
                content: content.chars().collect(),
            })
        })
        .collect()
}

/// An `aws_lb` listener as the ALB rules see it.
struct Listener {
    file: String,
    name: String,
    line: usize,
    port: Option<i128>,
    protocol: Option<String>,
    action_type: Option<String>,
    has_certificate_arn: bool,
}

/// True if any `aws_lb` is an internet-facing APPLICATION load balancer. Network
/// and gateway balancers have no HTTPS listener by design; an absent or
/// variable-driven type or `internal` flag is treated as internet-facing.
fn has_internet_facing_alb(files: &[TfFile]) -> bool {
    files
        .iter()
        .flat_map(|file| extract_blocks(&file.content, "aws_lb"))
        .any(|block| {
            let kind = attr_string(&block.body, "load_balancer_type");
            if matches!(kind.as_deref(), Some("network" | "gateway")) {
                return false;
            }
            matches!(
                attr_string(&block.body, "internal").as_deref(),
                None | Some("false")
            )
        })
}

fn parse_listeners(files: &[TfFile]) -> Vec<Listener> {
    let mut listeners = Vec::new();
    for file in files {
        for block in extract_blocks(&file.content, "aws_lb_listener") {
            listeners.push(Listener {
                file: file.rel.clone(),
                name: block.name,
                line: block.line,
                port: attr_int(&block.body, "port"),
                protocol: attr_string(&block.body, "protocol"),
                action_type: default_action_type(&block.body),
                has_certificate_arn: text(&block.body).contains("certificate_arn"),
            });
        }
    }
    listeners
}

fn check_alb_https_policy(files: &[TfFile]) -> Vec<Violation> {
    if !has_internet_facing_alb(files) {
        return Vec::new();
    }
    let listeners = parse_listeners(files);
    let mut violations = Vec::new();
    let protocol =
        |listener: &Listener| listener.protocol.clone().unwrap_or_default().to_uppercase();
    let https_ok = listeners.iter().any(|listener| {
        listener.port == Some(443)
            && protocol(listener) == "HTTPS"
            && listener.has_certificate_arn
            && listener.action_type.as_deref() == Some("forward")
    });
    if !https_ok {
        // Point at an aws_lb file when there is one, else the first file.
        let lb_file = files
            .iter()
            .find(|file| !extract_blocks(&file.content, "aws_lb").is_empty())
            .map_or_else(|| files[0].rel.as_str(), |file| file.rel.as_str());
        violations.push(Violation::new(
            "alb_https_listener",
            lb_file,
            0,
            "Internet-facing ALB requires an HTTPS listener on port 443 with certificate_arn \
             and a forward action"
                .to_string(),
            "Add an aws_lb_listener on port 443, protocol \"HTTPS\", with ssl_policy, \
             certificate_arn, and a forward default_action",
        ));
    }
    for listener in &listeners {
        if listener.port != Some(80) || protocol(listener) != "HTTP" {
            continue;
        }
        if listener.action_type.as_deref() == Some("forward") {
            violations.push(Violation::new(
                "alb_http_redirect",
                &listener.file,
                listener.line,
                format!(
                    "ALB HTTP listener '{}' on port 80 forwards to targets; it must redirect to HTTPS",
                    listener.name
                ),
                "Replace the forward default_action with a redirect block: type = \"redirect\", \
                 redirect { port = \"443\", protocol = \"HTTPS\", status_code = \"HTTP_301\" }",
            ));
        }
    }
    violations
}

/// RDS resources that literally set `publicly_accessible = true`.
fn check_rds_not_public(files: &[TfFile]) -> Vec<Violation> {
    let mut violations = Vec::new();
    for file in files {
        for kind in ["aws_db_instance", "aws_rds_cluster"] {
            for block in extract_blocks(&file.content, kind) {
                if attr_string(&block.body, "publicly_accessible").as_deref() == Some("true") {
                    violations.push(Violation::new(
                        "rds_not_public",
                        &file.rel,
                        block.line,
                        format!(
                            "{kind} '{}' sets publicly_accessible = true — the database is \
                             reachable from the internet",
                            block.name
                        ),
                        "Set publicly_accessible = false and place the database in private \
                         subnets; reach it from application security groups only",
                    ));
                }
            }
        }
    }
    violations
}

/// The subset of `ports` inside the rule's `[from_port, to_port]` range; empty
/// when the range is missing or not a literal.
fn ingress_covered_ports(ingress: &[char], ports: &[i128]) -> Vec<i128> {
    let (Some(from), Some(to)) = (attr_int(ingress, "from_port"), attr_int(ingress, "to_port"))
    else {
        return Vec::new();
    };
    ports
        .iter()
        .copied()
        .filter(|port| from <= *port && *port <= to)
        .collect()
}

/// True if `cidr` is a literal range covering the whole address space
/// (`ipaddress.ip_network(cidr, strict=False).prefixlen == 0`). Anything that
/// is not a parseable literal is false, which keeps the checks fail-open.
fn is_entire_internet(cidr: &str) -> bool {
    let Some((address, prefix)) = cidr.split_once('/') else {
        return false;
    };
    let zero = |digits: &str| !digits.is_empty() && digits.chars().all(|c| c == '0');
    if address.parse::<Ipv4Addr>().is_ok() {
        // `/0` or the equivalent netmask `/0.0.0.0`.
        return zero(prefix)
            || prefix
                .parse::<Ipv4Addr>()
                .is_ok_and(|mask| mask.is_unspecified());
    }
    address.parse::<Ipv6Addr>().is_ok() && zero(prefix)
}

/// `re.findall(r'"([^"]*)"', text)`.
fn quoted_strings(chars: &[char]) -> Vec<String> {
    let mut found = Vec::new();
    let mut pos = 0;
    while let Some(open) = chars[pos..].iter().position(|c| *c == '"') {
        let start = pos + open + 1;
        let Some(close) = chars[start..].iter().position(|c| *c == '"') else {
            break;
        };
        found.push(text(&chars[start..start + close]));
        pos = start + close + 1;
    }
    found
}

/// The "entire internet" CIDRs an ingress rule allows, IPv4 and IPv6 each read
/// under their own attribute, comments stripped by the shared lexer, only
/// quoted literals counted, values as spelled in the file.
fn ingress_public_cidrs(ingress: &[char]) -> Vec<String> {
    let body = strip_comments(ingress);
    let mut found: Vec<String> = Vec::new();
    for attr in ["cidr_blocks", "ipv6_cidr_blocks"] {
        let Some(inner) = attr_list_inner(&body, attr) else {
            continue;
        };
        for cidr in quoted_strings(&inner) {
            if is_entire_internet(&cidr) && !found.contains(&cidr) {
                found.push(cidr);
            }
        }
    }
    found
}

/// Inline `aws_security_group` ingress blocks: (file, name, line, ingress body).
fn inline_ingress(files: &[TfFile]) -> Vec<(&TfFile, String, usize, Vec<char>)> {
    let mut rules = Vec::new();
    for file in files {
        for block in extract_blocks(&file.content, "aws_security_group") {
            for ingress in own_blocks(&block.body, "ingress") {
                rules.push((file, block.name.clone(), block.line, ingress));
            }
        }
    }
    rules
}

/// Inline ingress that opens a database port to the whole internet. Separate
/// rule resources are not correlated (fail-open).
fn check_db_sg_no_public_ingress(files: &[TfFile]) -> Vec<Violation> {
    let mut violations = Vec::new();
    for (file, name, line, ingress) in inline_ingress(files) {
        let public = ingress_public_cidrs(&ingress);
        if !ingress_covered_ports(&ingress, &DB_PORTS).is_empty() && !public.is_empty() {
            violations.push(Violation::new(
                "db_sg_no_public_ingress",
                &file.rel,
                line,
                format!(
                    "aws_security_group '{name}' has an ingress rule that opens a database port \
                     (5432/3306) to {}",
                    public.join(" and ")
                ),
                "Restrict the ingress to the application security group (security_groups = \
                 [aws_security_group.app.id]) or a private CIDR — never 0.0.0.0/0 or ::/0 for a \
                 database port",
            ));
        }
    }
    violations
}

/// Inline ingress that opens an admin or datastore port to the whole internet.
fn check_sg_no_public_admin_ingress(files: &[TfFile]) -> Vec<Violation> {
    let ports: Vec<i128> = SENSITIVE_PORTS.iter().map(|(port, _)| *port).collect();
    let mut violations = Vec::new();
    for (file, name, line, ingress) in inline_ingress(files) {
        let public = ingress_public_cidrs(&ingress);
        if public.is_empty() {
            continue;
        }
        let mut hit = ingress_covered_ports(&ingress, &ports);
        hit.sort_unstable();
        hit.dedup();
        if hit.is_empty() {
            continue;
        }
        let labels: Vec<String> = hit
            .iter()
            .map(|port| {
                let label = SENSITIVE_PORTS
                    .iter()
                    .find(|(candidate, _)| candidate == port)
                    .map_or("", |(_, label)| *label);
                format!("{port} ({label})")
            })
            .collect();
        violations.push(Violation::new(
            "sg_no_public_admin_ingress",
            &file.rel,
            line,
            format!(
                "aws_security_group '{name}' opens sensitive port(s) {} to {}",
                labels.join(", "),
                public.join(" and ")
            ),
            "Restrict this ingress to a bastion/app security group or a private CIDR; never \
             expose admin or datastore ports to the internet",
        ));
    }
    violations
}

/// IAM `Allow` statements with an unsafe Action or Resource wildcard in the
/// literal policy JSON of `aws_iam_policy`, `aws_iam_role_policy` and the
/// group/user variants; `aws_iam_policy_document` data sources are not
/// inspected (fail-open).
fn check_no_wildcard_iam(files: &[TfFile]) -> Vec<Violation> {
    let mut violations = Vec::new();
    for file in files {
        for kind in [
            "aws_iam_policy",
            "aws_iam_role_policy",
            "aws_iam_group_policy",
            "aws_iam_user_policy",
        ] {
            for block in extract_blocks(&file.content, kind) {
                if !iam::has_allow_effect(&block.body) {
                    continue;
                }
                let without_allowed = iam::remove_allowed_resource_wildcards(&block.body);
                if iam::key_is_wildcard(&block.body, "Action")
                    || iam::key_is_wildcard(&without_allowed, "Resource")
                {
                    violations.push(Violation::new(
                        "no_wildcard_iam",
                        &file.rel,
                        block.line,
                        format!(
                            "{kind} '{}' grants Action \"*\" or Resource \"*\" in an Allow \
                             statement — over-broad permissions",
                            block.name
                        ),
                        "Scope the policy to specific actions and resource ARNs; isolate any \
                         documented action that requires Resource \"*\"",
                    ));
                }
            }
        }
    }
    violations
}

/// True when a boolean attribute is neither literally `true` nor
/// variable-driven: it is absent or literally `false`.
fn lacks_true(body: &[char], attr: &str) -> bool {
    let value = attr_string(body, attr);
    // Present but not a literal: variable-driven, fail open.
    if has_own_attr(body, attr) && value.is_none() {
        return false;
    }
    value.as_deref() != Some("true")
}

/// RDS resources without `storage_encrypted = true` (RDS defaults to
/// unencrypted).
fn check_rds_encryption_at_rest(files: &[TfFile]) -> Vec<Violation> {
    let mut violations = Vec::new();
    for file in files {
        for kind in ["aws_db_instance", "aws_rds_cluster"] {
            for block in extract_blocks(&file.content, kind) {
                if lacks_true(&block.body, "storage_encrypted") {
                    violations.push(Violation::new(
                        "rds_encryption_at_rest",
                        &file.rel,
                        block.line,
                        format!(
                            "{kind} '{}' does not set storage_encrypted = true — RDS storage \
                             defaults to unencrypted",
                            block.name
                        ),
                        "Set storage_encrypted = true (optionally with a kms_key_id)",
                    ));
                }
            }
        }
    }
    violations
}

/// `ElastiCache` replication groups without `at_rest_encryption_enabled = true`.
fn check_elasticache_encryption_at_rest(files: &[TfFile]) -> Vec<Violation> {
    let mut violations = Vec::new();
    for file in files {
        for block in extract_blocks(&file.content, "aws_elasticache_replication_group") {
            if lacks_true(&block.body, "at_rest_encryption_enabled") {
                violations.push(Violation::new(
                    "elasticache_encryption_at_rest",
                    &file.rel,
                    block.line,
                    format!(
                        "aws_elasticache_replication_group '{}' does not set \
                         at_rest_encryption_enabled = true",
                        block.name
                    ),
                    "Set at_rest_encryption_enabled = true (and consider \
                     transit_encryption_enabled = true for in-transit protection)",
                ));
            }
        }
    }
    violations
}

/// Standalone Redis `aws_elasticache_cluster` resources without
/// `transit_encryption_enabled = true`. Memcached, an absent or variable-driven
/// engine and members of a replication group are exempt.
fn check_elasticache_cluster_encryption(files: &[TfFile]) -> Vec<Violation> {
    let mut violations = Vec::new();
    for file in files {
        for block in extract_blocks(&file.content, "aws_elasticache_cluster") {
            if attr_string(&block.body, "engine").as_deref() != Some("redis")
                || has_own_attr(&block.body, "replication_group_id")
                || !lacks_true(&block.body, "transit_encryption_enabled")
            {
                continue;
            }
            violations.push(Violation::new(
                "elasticache_encryption_at_rest",
                &file.rel,
                block.line,
                format!(
                    "aws_elasticache_cluster '{}' (engine = redis) does not set \
                     transit_encryption_enabled = true",
                    block.name
                ),
                "Set transit_encryption_enabled = true on the Redis aws_elasticache_cluster. For \
                 at-rest encryption, move the cache into an aws_elasticache_replication_group \
                 with at_rest_encryption_enabled = true — at_rest_encryption_enabled is not a \
                 valid argument on aws_elasticache_cluster.",
            ));
        }
    }
    violations
}

/// Runs every rule over `files`; no `.tf` file at all is its own violation.
pub(crate) fn validate(files: &[TfFile]) -> Vec<Violation> {
    if files.is_empty() {
        return vec![Violation::new(
            "no_tf_files",
            ".",
            0,
            "No .tf files found in terraform directory".to_string(),
            "Ensure the generate step wrote terraform/ before validation",
        )];
    }
    let mut violations = check_alb_https_policy(files);
    violations.extend(check_rds_not_public(files));
    violations.extend(check_db_sg_no_public_ingress(files));
    violations.extend(check_sg_no_public_admin_ingress(files));
    violations.extend(check_no_wildcard_iam(files));
    violations.extend(check_rds_encryption_at_rest(files));
    violations.extend(check_elasticache_encryption_at_rest(files));
    violations.extend(check_elasticache_cluster_encryption(files));
    violations
}

/// The `--json` verdict document.
fn report(violations: &[Violation]) -> Json {
    Json::object(vec![
        ("check", Json::string("policy")),
        (
            "policy_status",
            Json::string(if violations.is_empty() {
                "POLICY_OK"
            } else {
                "POLICY_FAIL"
            }),
        ),
        (
            "violations",
            Json::Array(violations.iter().map(Violation::as_json).collect()),
        ),
    ])
}

/// Handler for `startup terraform-policy DIR [--json FILE]`.
///
/// # Errors
///
/// A usage error (exit status 2) without `DIR`, when `DIR` is not a directory
/// or when the `--json` file cannot be written; a runtime error (exit status
/// 1) when a file cannot be read or any policy is violated.
pub fn terraform_policy_command(
    invocation: &Invocation,
    output: &mut Output<'_>,
) -> Result<(), ToolkitError> {
    let Some(dir) = invocation.positional(0) else {
        return Err(ToolkitError::usage(
            "missing_argument",
            "the following arguments are required: terraform_dir",
        ));
    };
    let dir: PathBuf = resolve_lenient(Path::new(dir));
    if !dir.is_dir() {
        output.diagnostic(&format!(
            "POLICY_FAIL | path={} | reason=not_a_directory",
            neutralize_line(&crate::safe_text::shown_path(&dir))
        ));
        return Err(ToolkitError::usage(
            "terraform_not_a_directory",
            format!(
                "terraform directory is not a directory: {}",
                crate::safe_text::shown_path(&dir)
            ),
        ));
    }
    let files = read_tf_files(&dir)
        .map_err(|message| ToolkitError::runtime("terraform_input_invalid", message))?;
    let violations = validate(&files);

    if let Some(path) = invocation.value("--json") {
        let document = dumps(&report(&violations), Some(2), false) + "\n";
        if let Err(error) = fs::write(path, document) {
            output.diagnostic(&format!(
                "POLICY_FAIL | reason=json_write_failed | detail={error}"
            ));
            return Err(ToolkitError::usage(
                "terraform_json_write_failed",
                format!("cannot write {path}: {error}"),
            ));
        }
    }
    if violations.is_empty() {
        return output.line(&format!("POLICY_OK | checks={CHECKS}"));
    }
    output.diagnostic(&format!("POLICY_FAIL | checks={CHECKS}"));
    for violation in &violations {
        output.diagnostic(&format!(
            "POLICY_FAIL | file={} | line={} | rule={} | reason={}",
            violation.file, violation.line, violation.rule, violation.summary
        ));
    }
    Err(ToolkitError::runtime(
        "terraform_policy_failed",
        format!("{} policy violation(s)", violations.len()),
    ))
}

//! Offline process contracts for the S2b renderer. No payload is executed.
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicUsize, Ordering},
};

static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "payload-render-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn write(&self, name: &str, text: &str) -> PathBuf {
        let path = self.0.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, text).unwrap();
        path
    }
    fn render(&self, template: &Path, mode: &str, extra: &[&str]) -> std::process::Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_toolkit"));
        cmd.args(["ai-payloads", "render"])
            .arg(template)
            .args(["--mode", mode, "--project-dir"])
            .arg(&self.0)
            .args(["--name", "example"])
            .args(extra);
        cmd.output().unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
const TEMPLATE: &str = "# Cell 0 [markdown]: Example\n# Description\n\n# Cell 1: Config\nACCEPT_EULA = [ACCEPT_EULA]\nrole = \"[ROLE_ARN]\"\n";

#[test]
fn ap_typ_01_04_06_07_valid_literals_golden() {
    let f = Fixture::new();
    let template = f.write("template.py", "# Cell 0 [markdown]: Example\n# Desc\n# Cell 1: Config\nACCEPT_EULA = [ACCEPT_EULA]\nflag = [TRUE_OR_FALSE]\na = [INPUT_TOKENS_STDDEV]\nb = [OUTPUT_TOKENS_STDDEV]\nmetrics = [METRICS_LIST]\n");
    let values = f.write("values.json", r#"{"values":{"ACCEPT_EULA":false,"TRUE_OR_FALSE":true,"INPUT_TOKENS_STDDEV":0,"OUTPUT_TOKENS_STDDEV":0,"METRICS_LIST":["Correctness","Faithfulness"]}}"#);
    assert!(f
        .render(&template, "script", &["--values", values.to_str().unwrap()])
        .status
        .success());
    let script = fs::read_to_string(f.0.join("scripts/01_example.py")).unwrap();
    for expected in [
        "ACCEPT_EULA = False",
        "flag = True",
        "a = 0",
        "b = 0",
        "metrics = [\"Correctness\", \"Faithfulness\"]",
    ] {
        assert!(script.contains(expected));
    }
    let values = f.write("values.json", r#"{"values":{"ACCEPT_EULA":true,"TRUE_OR_FALSE":false,"INPUT_TOKENS_STDDEV":0,"OUTPUT_TOKENS_STDDEV":0,"METRICS_LIST":[]}}"#);
    assert!(f
        .render(
            &template,
            "script",
            &[
                "--values",
                values.to_str().unwrap(),
                "--consent",
                "ACCEPT_EULA"
            ]
        )
        .status
        .success());
    let script = fs::read_to_string(f.0.join("scripts/02_example.py")).unwrap();
    for expected in ["ACCEPT_EULA = True", "flag = False", "metrics = []"] {
        assert!(script.contains(expected));
    }
    let names = (0..65)
        .map(|i| format!("\"M{i}\""))
        .collect::<Vec<_>>()
        .join(",");
    let values = f.write("values.json", &format!("{{\"values\":{{\"ACCEPT_EULA\":false,\"TRUE_OR_FALSE\":false,\"INPUT_TOKENS_STDDEV\":0,\"OUTPUT_TOKENS_STDDEV\":0,\"METRICS_LIST\":[{names}]}}}}"));
    assert_eq!(
        f.render(&template, "script", &["--values", values.to_str().unwrap()])
            .status
            .code(),
        Some(1)
    );
}

#[test]
fn ap_con_05_template_shaped_consent_sites_require_consent() {
    for line in [
        "model.accept_eula = [ACCEPT_EULA]",
        "ACCEPT_EULA = [ACCEPT_EULA]\nmodel = {'accept_eula': ACCEPT_EULA}",
    ] {
        let f = Fixture::new();
        let template = f.write(
            "template.py",
            &format!("# Cell 0 [markdown]: Example\n# Desc\n# Cell 1: Code\n{line}\n"),
        );
        let values = f.write("values.json", r#"{"values":{"ACCEPT_EULA":true}}"#);
        let result = f.render(&template, "script", &["--values", values.to_str().unwrap()]);
        assert_eq!(result.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&result.stderr).contains("consent_not_given"));
        assert!(f
            .render(
                &template,
                "script",
                &[
                    "--values",
                    values.to_str().unwrap(),
                    "--consent",
                    "ACCEPT_EULA"
                ]
            )
            .status
            .success());
    }
}

#[test]
fn ap_app_01_02_exact_report_and_ignored_names() {
    let f = Fixture::new();
    let template = f.write(
        "template.py",
        "# Cell 0 [markdown]: Example\n# Desc\n# Cell 1: Code\npass\n",
    );
    let source = f.write(
        "notebooks/example.ipynb",
        r#"{"cells":[],"metadata":{"keep":true},"nbformat":4,"nbformat_minor":4}"#,
    );
    for name in [
        "example-1.ipynb",
        "example-001.ipynb",
        "example-01.ipynb.bak",
    ] {
        f.write(&format!("notebooks/{name}"), "ignored");
    }
    let result = f.render(&template, "notebook", &["--append"]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let out = String::from_utf8_lossy(&result.stdout);
    assert!(out.contains(&format!(
        "source: {} (sha256 4c27e216c25086bdcc597c7ee7c4dbc415899a8ac245fc713f2776efd9dcca53)",
        source.display()
    )));
    assert!(out.contains(&format!(
        "open: {}",
        f.0.join("notebooks/example-01.ipynb").display()
    )));
}

#[cfg(unix)]
#[test]
fn ap_app_07_fifo_source_is_refused() {
    use std::process::Command;
    let f = Fixture::new();
    let template = f.write(
        "template.py",
        "# Cell 0 [markdown]: Example\n# Desc\n# Cell 1: Code\npass\n",
    );
    fs::create_dir(f.0.join("notebooks")).unwrap();
    let fifo = f.0.join("notebooks/example.ipynb");
    assert!(Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap()
        .success());
    let result = f.render(&template, "notebook", &["--append"]);
    assert_eq!(result.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&result.stderr).contains("append_source_invalid"));
    assert!(!f.0.join("notebooks/example-01.ipynb").exists());
}

#[test]
fn ap_asn_02_04_06_target_and_secret_matrix() {
    for (source, name) in [
        (
            "# Cell 0 [markdown]: Example\n# TARGET = 'old'\n# Cell 1: Code\npass\n",
            "TARGET",
        ),
        (
            "# Cell 0 [markdown]: Example\n# Desc\n# Cell 1: Code\nTARGET, other = 'old', 1\n",
            "TARGET",
        ),
        (
            "# Cell 0 [markdown]: Example\n# Desc\n# Cell 1: Code\nTARGET = f'old'\n",
            "TARGET",
        ),
        (
            "# Cell 0 [markdown]: Example\n# Desc\n# Cell 1: Code\nTARGET = None\n",
            "TARGET",
        ),
        (
            "# Cell 0 [markdown]: Example\n# Desc\n# Cell 1: Code\nS3_OUTPUT_PATH = f'old'\n",
            "S3_OUTPUT_PATH",
        ),
    ] {
        let f = Fixture::new();
        let template = f.write("template.py", source);
        let values = f.write(
            "values.json",
            &format!("{{\"assign\":{{\"{name}\":\"new\"}}}}"),
        );
        let result = f.render(&template, "script", &["--values", values.to_str().unwrap()]);
        assert_eq!(result.status.code(), Some(1), "{source}");
        assert!(String::from_utf8_lossy(&result.stderr).contains("assign_target_invalid"));
    }
    for (value, expected, success) in [
        ("null", "HF_HUB_TOKEN = None", true),
        (
            "\"arn:aws:secretsmanager:us-east-1:123456789012:secret:example\"",
            "arn:aws:secretsmanager",
            true,
        ),
        ("\"literal-secret-sentinel\"", "secret_value_invalid", false),
    ] {
        let f = Fixture::new();
        let template = f.write("template.py", "# Cell 0 [markdown]: Example\n# Desc\n# Cell 1: Code\nHF_HUB_TOKEN = ''\nreward_function_path = (\n    'old'\n)\n");
        let values = f.write(
            "values.json",
            &format!(
                "{{\"assign\":{{\"HF_HUB_TOKEN\":{value},\"reward_function_path\":\"new\"}}}}"
            ),
        );
        let result = f.render(&template, "script", &["--values", values.to_str().unwrap()]);
        assert_eq!(
            result.status.success(),
            success,
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        if success {
            let script = fs::read_to_string(f.0.join("scripts/01_example.py")).unwrap();
            assert!(script.contains(expected));
            assert!(script.contains("reward_function_path = (\n    'new'\n)"));
        } else {
            assert!(String::from_utf8_lossy(&result.stderr).contains(expected));
            assert!(!String::from_utf8_lossy(&result.stdout).contains("literal-secret-sentinel"));
            assert!(!String::from_utf8_lossy(&result.stderr).contains("literal-secret-sentinel"));
            assert!(!f.0.join("scripts").exists());
        }
    }
}

#[test]
#[allow(clippy::too_many_lines)] // Explicit per-type process rejection matrix.
fn ap_typ_03_04_06_07_sub_14_process_rejection_matrix() {
    for (name, cases) in [
        (
            "ACCEPT_EULA",
            vec![
                "\"true\"",
                "\"false\"",
                "1",
                "0",
                "null",
                "[]",
                "[true]",
                "[\"false\"]",
                "{}",
            ],
        ),
        (
            "TRUE_OR_FALSE",
            vec!["\"false\"", "1", "null", "[\"false\"]"],
        ),
        (
            "CONCURRENCY",
            vec![
                "0",
                "-1",
                "512.0",
                "5.12e2",
                "2147483648",
                "true",
                "\"512\"",
                "null",
                "[512]",
                "{}",
                "NaN",
            ],
        ),
        (
            "INPUT_TOKENS_MEAN",
            vec![
                "0",
                "-1",
                "512.0",
                "5.12e2",
                "2147483648",
                "true",
                "\"512\"",
                "null",
                "[512]",
            ],
        ),
        (
            "OUTPUT_TOKENS_MEAN",
            vec![
                "0",
                "-1",
                "512.0",
                "5.12e2",
                "2147483648",
                "true",
                "\"512\"",
                "null",
                "[512]",
            ],
        ),
        (
            "REQUEST_COUNT",
            vec![
                "0",
                "-1",
                "512.0",
                "5.12e2",
                "2147483648",
                "true",
                "\"512\"",
                "null",
                "[512]",
            ],
        ),
        (
            "INPUT_TOKENS_STDDEV",
            vec![
                "-1",
                "512.0",
                "5.12e2",
                "2147483648",
                "true",
                "\"512\"",
                "null",
                "[512]",
            ],
        ),
        (
            "OUTPUT_TOKENS_STDDEV",
            vec![
                "-1",
                "512.0",
                "5.12e2",
                "2147483648",
                "true",
                "\"512\"",
                "null",
                "[512]",
            ],
        ),
        (
            "METRICS_LIST",
            vec![
                "null",
                "\"Correctness\"",
                "[1]",
                "[\"\"]",
                "[\"a b\"]",
                "[\"X\",\"X\"]",
                "[[\"X\"]]",
                "{}",
            ],
        ),
    ] {
        let mut diagnostic = None;
        for raw in cases {
            let f = Fixture::new();
            let template = f.write(
                "template.py",
                &format!("# Cell 0 [markdown]: Example\n# Desc\n# Cell 1: Code\nx = [{name}]\n"),
            );
            let values = f.write(
                "values.json",
                &format!("{{\"values\":{{\"{name}\":{raw}}}}}"),
            );
            let result = f.render(&template, "script", &["--values", values.to_str().unwrap()]);
            assert_eq!(result.status.code(), Some(1), "{name}: {raw}");
            if raw != "NaN" {
                let stderr = String::from_utf8_lossy(&result.stderr);
                assert!(stderr.contains(name), "{name}: {raw}: {stderr}");
                assert!(
                    stderr.contains("typed_value_invalid"),
                    "{name}: {raw}: {stderr}"
                );
                // Identical diagnostics for different values prove that even
                // short values such as 0 or null are not interpolated.
                if let Some(previous) = &diagnostic {
                    assert_eq!(&result.stderr, previous, "{name}: {raw}");
                } else {
                    diagnostic = Some(result.stderr.clone());
                }
                assert!(result.stdout.is_empty(), "{name}: {raw}");
            }
            assert!(!f.0.join("scripts").exists());
        }
    }
}

#[test]
fn ap_fs_01_cli_03_reserved_name_and_exit_codes() {
    let f = Fixture::new();
    let template = f.write(
        "template.py",
        "# Cell 0 [markdown]: Example\n# Desc\n# Cell 1: Code\npass\n",
    );
    let project = f.0.join("reserved-01");
    fs::create_dir(&project).unwrap();
    for args in [vec!["--name", "example-01"], vec![]] {
        let result = Command::new(env!("CARGO_BIN_EXE_toolkit"))
            .args(["ai-payloads", "render"])
            .arg(&template)
            .args(["--mode", "script", "--project-dir"])
            .arg(&project)
            .args(args)
            .output()
            .unwrap();
        assert_eq!(result.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&result.stderr).contains("name_reserved_suffix"));
    }
    assert_eq!(f.render(&template, "script", &[]).status.code(), Some(0));
    assert_eq!(f.render(&template, "invalid", &[]).status.code(), Some(2));
}

#[test]
fn ap_sub_11_22_sec_01_process_boundaries() {
    for raw in [
        "\"line\\nfeed\"",
        "\"escape\\u001b\"",
        "\"bidi\\u202e\"",
        &format!("\"{}\"", "x".repeat(4097)),
    ] {
        let f = Fixture::new();
        let template = f.write(
            "template.py",
            "# Cell 0 [markdown]: Example\n# Desc\n# Cell 1: Code\nx = '[VALUE]'\n",
        );
        let values = f.write(
            "values.json",
            &format!("{{\"values\":{{\"VALUE\":{raw}}}}}"),
        );
        let result = f.render(&template, "script", &["--values", values.to_str().unwrap()]);
        assert_eq!(result.status.code(), Some(1));
        assert!(!f.0.join("scripts").exists());
    }
    let f = Fixture::new();
    let template = f.write(
        "template.py",
        "# Cell 0 [markdown]: Example\n# Desc\n# Cell 1: Code\n# [COMMENT_ONLY]\npass\n",
    );
    assert!(f.render(&template, "script", &[]).status.success());
    let f = Fixture::new();
    let template = f.write(
        "template.py",
        "# Cell 0 [markdown]: Example\n# Desc\n# Cell 1: Code\nx = '[HF_TOKEN]'\n",
    );
    let values = f.write(
        "values.json",
        r#"{"values":{"HF_TOKEN":"literal-secret-sentinel"}}"#,
    );
    for mode in ["script", "notebook"] {
        let result = f.render(&template, mode, &["--values", values.to_str().unwrap()]);
        assert_eq!(result.status.code(), Some(1));
        for bytes in [&result.stdout, &result.stderr] {
            assert!(!String::from_utf8_lossy(bytes).contains("literal-secret-sentinel"));
        }
        for dir in ["scripts", "notebooks"] {
            let dir = f.0.join(dir);
            if dir.exists() {
                for entry in fs::read_dir(dir).unwrap() {
                    let path = entry.unwrap().path();
                    assert!(!String::from_utf8_lossy(&fs::read(path).unwrap())
                        .contains("literal-secret-sentinel"));
                }
            }
        }
    }
}

#[test]
fn ap_con_01_missing_typed_consent_is_refused_even_when_unresolved_allowed() {
    for mode in ["notebook", "script"] {
        let f = Fixture::new();
        let template = f.write("template.py", TEMPLATE);
        let result = f.render(&template, mode, &["--allow-unresolved"]);
        assert_eq!(result.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&result.stderr).contains("typed_slot_unresolved"));
        assert!(!f.0.join("notebooks/example.ipynb").exists());
        assert!(!f.0.join("scripts/01_example.py").exists());
    }
}

#[test]
fn ap_typ_01_02_consent_requires_explicit_flag_and_emits_boolean() {
    let f = Fixture::new();
    let template = f.write("template.py", TEMPLATE);
    let values = f.write(
        "values.json",
        r#"{"values":{"ACCEPT_EULA":true,"ROLE_ARN":"role"}}"#,
    );
    let value_path = values.to_str().unwrap();
    let denied = f.render(&template, "script", &["--values", value_path]);
    assert_eq!(denied.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&denied.stderr).contains("consent_not_given"));
    assert!(!f.0.join("scripts/01_example.py").exists());
    let allowed = f.render(
        &template,
        "script",
        &["--values", value_path, "--consent", "ACCEPT_EULA"],
    );
    assert!(
        allowed.status.success(),
        "{}",
        String::from_utf8_lossy(&allowed.stderr)
    );
    assert!(fs::read_to_string(f.0.join("scripts/01_example.py"))
        .unwrap()
        .contains("ACCEPT_EULA = True"));
}

#[test]
fn ap_app_01_historical_consent_is_preserved_and_new_false_needs_no_consent() {
    let f = Fixture::new();
    let template = f.write("template.py", TEMPLATE);
    let values = f.write(
        "values.json",
        r#"{"values":{"ACCEPT_EULA":false,"ROLE_ARN":"role"}}"#,
    );
    let source = f.write("notebooks/example.ipynb", r#"{"cells":[{"cell_type":"code","execution_count":null,"metadata":{},"outputs":[],"source":["ACCEPT_EULA = True\n","ACCEPT_EULA = historical_expression\n"]}],"metadata":{},"nbformat":4,"nbformat_minor":4}"#);
    let original = fs::read(&source).unwrap();
    let result = f.render(
        &template,
        "notebook",
        &["--values", values.to_str().unwrap(), "--append"],
    );
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(fs::read(source).unwrap(), original);
    let copy = fs::read_to_string(f.0.join("notebooks/example-01.ipynb")).unwrap();
    assert!(copy.contains("ACCEPT_EULA = True"));
    assert!(copy.contains("ACCEPT_EULA = historical_expression"));
    assert!(copy.contains("ACCEPT_EULA = False"));
    let out = String::from_utf8_lossy(&result.stdout);
    assert!(out.contains("source:") && out.contains("sha256") && out.contains("open:"));
}

#[test]
fn ap_pub_15_dry_run_creates_no_output() {
    let f = Fixture::new();
    let template = f.write("template.py", TEMPLATE);
    let values = f.write(
        "values.json",
        r#"{"values":{"ACCEPT_EULA":false,"ROLE_ARN":"role"}}"#,
    );
    let result = f.render(
        &template,
        "notebook",
        &["--values", values.to_str().unwrap(), "--dry-run"],
    );
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(!f.0.join("notebooks/example.ipynb").exists());
}

#[test]
fn ap_t28_03_normalized_template_path_requires_module_cell() {
    let f = Fixture::new();
    f.write("dataset-transformation/code_templates/transformation.py", "# Cell 0 [markdown]: Example\n# Description\n# Cell 1: Code\npass\n# Cell 2: Module\npass\n");
    let template =
        f.0.join("dataset-transformation/code_templates/../code_templates/transformation.py");
    let result = f.render(&template, "script", &[]);
    assert_eq!(result.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&result.stderr).contains("module_cell_required"));
}

#[test]
fn ap_scr_04_nonregular_destination_has_specific_error() {
    let f = Fixture::new();
    let template = f.write(
        "template.py",
        "# Cell 0 [markdown]: Example\n# Description\n# Cell 1: Code\npass\n",
    );
    fs::create_dir_all(f.0.join("scripts/01_example.py")).unwrap();
    let result = f.render(&template, "script", &[]);
    assert_eq!(result.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&result.stderr).contains("script_destination_not_regular"));
    assert!(f.0.join("scripts/01_example.py").is_dir());
}

#[test]
fn ap_asn_01_float_assignments_keep_float_literals() {
    for (value, expected) in [
        ("1", "1.0"),
        ("1.0", "1.0"),
        ("1e300", "1e300"),
        ("1e-7", "1e-7"),
    ] {
        let f = Fixture::new();
        let template = f.write(
            "template.py",
            "# Cell 0 [markdown]: Example\n# Description\n# Cell 1: Code\nLR = 0.5\n",
        );
        let values = f.write("values.json", &format!("{{\"assign\":{{\"LR\":{value}}}}}"));
        let result = f.render(&template, "script", &["--values", values.to_str().unwrap()]);
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let script = fs::read_to_string(f.0.join("scripts/01_example.py")).unwrap();
        assert!(script.contains(&format!("LR = {expected}\n")), "{script}");
    }
}

#[test]
fn ap_sub_magic_lines_reject_quoted_slots() {
    for line in ["!aws s3 cp \"[S3_URI]\" .", "  %run \"[S3_URI]\""] {
        let f = Fixture::new();
        let template = f.write(
            "template.py",
            &format!("# Cell 0 [markdown]: Example\n# Description\n# Cell 1: Code\n{line}\n"),
        );
        let values = f.write("values.json", r#"{"values":{"S3_URI":"$(id)"}}"#);
        let result = f.render(
            &template,
            "notebook",
            &["--values", values.to_str().unwrap()],
        );
        assert_eq!(result.status.code(), Some(1), "{line}");
        assert!(!f.0.join("notebooks/example.ipynb").exists());
    }
}

#[test]
fn ap_con_05_quoted_and_lowercase_consent_are_refused() {
    for (source, values) in [
        (
            "model.accept_eula = \"[ACCEPT_EULA]\"\n",
            r#"{"values":{"ACCEPT_EULA":"true"}}"#,
        ),
        (
            "accept_eula = False\n",
            r#"{"assign":{"accept_eula":true}}"#,
        ),
    ] {
        let f = Fixture::new();
        let template = f.write(
            "template.py",
            &format!("# Cell 0 [markdown]: Example\n# Description\n# Cell 1: Code\n{source}"),
        );
        let values = f.write("values.json", values);
        let result = f.render(&template, "script", &["--values", values.to_str().unwrap()]);
        assert_eq!(result.status.code(), Some(1), "{source}");
        assert!(!f.0.join("scripts/01_example.py").exists());
    }
}

#[test]
fn ap_con_06_normalized_and_fstring_consent_identifiers_are_guarded() {
    for code in [
        "ＡCCEPT_EULA = True\n",
        "print(f\"{(ACCEPT_EULA := True)}\")\n",
    ] {
        let f = Fixture::new();
        let template = f.write(
            "template.py",
            "# Cell 0 [markdown]: Example\n# Description\n# Cell 1: Code\npass\n",
        );
        let cell = f.write("cell.py", code);
        let arg = format!("1={}", cell.display());
        let result = f.render(&template, "script", &["--cell", &arg]);
        assert_eq!(result.status.code(), Some(1), "{code}");
        assert!(String::from_utf8_lossy(&result.stderr).contains("cell_binds_consent"));
    }
}

#[test]
fn ap_con_06_cell_consent_identifier_guard() {
    let f = Fixture::new();
    let template = f.write(
        "template.py",
        "# Cell 0 [markdown]: Example\n# Description\n# Cell 1: Code\npass\n",
    );
    for code in ["ACCEPT_EULA = True\n", "x = [ACCEPT_EULA]\n"] {
        let cell = f.write("cell.py", code);
        let arg = format!("1={}", cell.display());
        let result = f.render(&template, "script", &["--cell", &arg]);
        assert_eq!(result.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&result.stderr).contains("cell_binds_consent"));
    }
    let cell = f.write("cell.py", "# ACCEPT_EULA\nx = 'ACCEPT_EULA'\n");
    let arg = format!("1={}", cell.display());
    assert!(f
        .render(&template, "script", &["--cell", &arg])
        .status
        .success());
    let f = Fixture::new();
    let template = f.write("template.py", "# Cell 0 [markdown]: Example\n# Description\n# Cell 1: Code\nACCEPT_EULA = False\nACCEPT_EULA = False\n");
    let cell = f.write("cell.py", "pass\n");
    let arg = format!("1={}", cell.display());
    let result = f.render(&template, "script", &["--cell", &arg]);
    assert!(String::from_utf8_lossy(&result.stderr).contains("cell_replaces_consent"));
}

#[test]
fn ap_con_06_exec_caller_code_boundary_is_syntactic() {
    let f = Fixture::new();
    let template = f.write(
        "template.py",
        "# Cell 0 [markdown]: Example\n# Description\n# Cell 1: Code\npass\n",
    );
    // The guard does not execute or interpret strings passed to exec. Caller
    // code remains trusted and the rendered bytes demonstrate that boundary.
    let body = "exec('ACCEPT_EULA = True')\n";
    let cell = f.write("cell.py", body);
    let arg = format!("1={}", cell.display());
    let result = f.render(&template, "script", &["--cell", &arg]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(fs::read_to_string(f.0.join("scripts/01_example.py"))
        .unwrap()
        .contains(body));
}

#[test]
fn ap_con_05_parenthesized_and_training_assignments_emit_booleans() {
    for source in [
        "ACCEPT_EULA = (\n    False  # license\n)",
        "ACCEPT_EULA = False\nmodel = train(accept_eula=ACCEPT_EULA)",
    ] {
        for (value, consent, expected) in [(false, false, "False"), (true, true, "True")] {
            for mode in ["script", "notebook"] {
                let f = Fixture::new();
                let template = f.write(
                    "template.py",
                    &format!("# Cell 0 [markdown]: Example\n# Desc\n# Cell 1: Code\n{source}\n"),
                );
                let values = f.write(
                    "values.json",
                    &format!("{{\"assign\":{{\"ACCEPT_EULA\":{value}}}}}"),
                );
                let mut args = vec!["--values", values.to_str().unwrap()];
                if consent {
                    args.extend(["--consent", "ACCEPT_EULA"]);
                }
                let result = f.render(&template, mode, &args);
                assert!(
                    result.status.success(),
                    "{}",
                    String::from_utf8_lossy(&result.stderr)
                );
                let path = if mode == "script" {
                    "scripts/01_example.py"
                } else {
                    "notebooks/example.ipynb"
                };
                let output = fs::read_to_string(f.0.join(path)).unwrap();
                assert!(
                    output.contains(&format!("{expected}  # license"))
                        || output.contains(&format!("ACCEPT_EULA = {expected}")),
                    "{mode}: {output}"
                );
            }
        }
    }
}

#[test]
fn ap_cat_10_all_real_templates_render_in_both_modes() {
    use toolkit_core::{parse_json, Json};
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/core-skills/aws-ai-ml");
    let catalog = Command::new(env!("CARGO_BIN_EXE_toolkit"))
        .args(["ai-payloads", "templates", "--json", "--root"])
        .arg(&root)
        .output()
        .unwrap();
    assert!(catalog.status.success());
    let catalog = parse_json(std::str::from_utf8(&catalog.stdout).unwrap()).unwrap();
    let Json::Array(entries) = catalog.get("templates").unwrap() else {
        panic!("catalog")
    };
    assert_eq!(entries.len(), 23);
    for entry in entries {
        let rel = entry.get("path").unwrap().as_str().unwrap();
        let Json::Array(placeholders) = entry.get("placeholders").unwrap() else {
            panic!("slots")
        };
        let template_text = fs::read_to_string(root.join("references").join(rel)).unwrap();
        let is_site = |line: &str| {
            let line = line.trim_start();
            line.starts_with("ACCEPT_EULA =") || line.starts_with("\"accept_eula\":")
        };
        let expected_sites = match rel {
            "model-deployment/code_templates/deploy-oss-sagemaker.py"
            | "model-deployment/code_templates/deploy-jumpstart-sagemaker.py"
            | "finetuning/code_templates/rlaif_builtin.py"
            | "finetuning/code_templates/rlaif_custom_prompt.py"
            | "finetuning/code_templates/sft.py"
            | "finetuning/code_templates/rlvr.py"
            | "finetuning/code_templates/dpo.py"
            | "training-jobs/code_templates/jumpstart_training.py" => 1,
            _ => 0,
        };
        assert_eq!(
            template_text.lines().filter(|line| is_site(line)).count(),
            expected_sites,
            "consent inventory: {rel}"
        );
        for mode in ["script", "notebook"] {
            let f = Fixture::new();
            let mut pairs = Vec::new();
            for slot in placeholders {
                if slot.get("code") == Some(&Json::Int(0)) {
                    continue;
                }
                let name = slot.get("name").unwrap().as_str().unwrap();
                let kind = slot.get("kind").unwrap().as_str().unwrap();
                let value = match kind {
                    "secret" => Json::Null,
                    "string" => Json::string("x"),
                    "value" => match name {
                        "ACCEPT_EULA" | "TRUE_OR_FALSE" => Json::Bool(false),
                        "METRICS_LIST" => Json::Array(vec![]),
                        _ => Json::Int(1),
                    },
                    _ => panic!("kind"),
                };
                pairs.push((name.to_owned(), value));
            }
            let values = f.write(
                "values.json",
                &Json::object(vec![("values", Json::Object(pairs))]).to_compact_string(),
            );
            let module = f.write(
                "scripts/transform_fn.py",
                "def transform_dataset(df):\n    return df\n",
            );
            let cell = format!("2={}", module.display());
            let args = if rel.ends_with("dataset-transformation/code_templates/transformation.py") {
                vec!["--values", values.to_str().unwrap(), "--cell", &cell]
            } else {
                vec!["--values", values.to_str().unwrap()]
            };
            let result = f.render(&root.join("references").join(rel), mode, &args);
            assert!(
                result.status.success(),
                "{rel} {mode}: {}",
                String::from_utf8_lossy(&result.stderr)
            );
            let code = if mode == "notebook" {
                let notebook =
                    parse_json(&fs::read_to_string(f.0.join("notebooks/example.ipynb")).unwrap())
                        .unwrap();
                let Json::Array(cells) = notebook.get("cells").unwrap() else {
                    panic!("cells")
                };
                cells
                    .iter()
                    .filter(|cell| cell.get("cell_type").and_then(Json::as_str) == Some("code"))
                    .map(|cell| {
                        let Json::Array(source) = cell.get("source").unwrap() else {
                            panic!("source")
                        };
                        source
                            .iter()
                            .map(|line| line.as_str().unwrap())
                            .collect::<String>()
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            } else {
                fs::read_to_string(f.0.join("scripts/01_example.py")).unwrap()
            };
            let sites: Vec<_> = code.lines().filter(|line| is_site(line)).collect();
            assert_eq!(
                sites.len(),
                expected_sites,
                "emitted consent inventory: {rel} {mode}"
            );
            for site in sites {
                let line = site.trim_start();
                if line.starts_with("ACCEPT_EULA = (") {
                    assert!(code.contains("ACCEPT_EULA = (\n    False"), "{rel} {mode}");
                } else {
                    assert!(
                        line.starts_with("ACCEPT_EULA = False")
                            || line.starts_with("\"accept_eula\": False"),
                        "{rel} {mode}: {line}"
                    );
                }
            }
            assert!(
                !code
                    .lines()
                    .filter(|line| !line.trim_start().starts_with('#'))
                    .any(|line| line.contains("[ACCEPT_EULA]")),
                "{rel} {mode}"
            );
            if mode == "script" {
                let output = fs::read_to_string(f.0.join("scripts/01_example.py")).unwrap();
                assert!(output.contains("# %% Cell 1:"), "{rel}");
                assert!(!output.contains("# NOTEBOOK_ONLY"), "{rel}");
                for line in template_text
                    .lines()
                    .filter(|line| line.trim_end().ends_with("# NOTEBOOK_ONLY"))
                {
                    let dropped = line
                        .trim_end()
                        .strip_suffix("# NOTEBOOK_ONLY")
                        .unwrap()
                        .trim_end();
                    assert!(
                        !output.lines().any(|emitted| emitted.trim_end() == dropped),
                        "{rel}: {dropped}"
                    );
                }
                for line in template_text
                    .lines()
                    .filter(|line| line.contains("# NOTEBOOK_ONLY_SECTION"))
                {
                    let number = line
                        .strip_prefix("# Cell ")
                        .unwrap()
                        .split(':')
                        .next()
                        .unwrap();
                    assert!(
                        !output.contains(&format!("# %% Cell {number}:")),
                        "{rel}: {line}"
                    );
                }
                for line in output
                    .lines()
                    .filter(|line| !line.trim_start().starts_with('#'))
                {
                    assert!(!line.contains("[ACCEPT_EULA]"), "{rel}: {line}");
                }
            }
        }
    }
}

#[test]
fn ap_cell_04_08_multiline_normalization_and_unsafe_characters() {
    let template_text = "# Cell 0 [markdown]: Example\n# Description\n# Cell 1: Code\npass\n";
    let body = "def transform_dataset(df):\n    x = 1\n\n\treturn df # comment";
    let mut outputs = Vec::new();
    for contents in [body.to_owned(), body.replace('\n', "\r\n")] {
        let f = Fixture::new();
        let template = f.write("template.py", template_text);
        let cell = f.write("cell.py", &contents);
        let arg = format!("1={}", cell.display());
        let result = f.render(&template, "script", &["--cell", &arg]);
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        outputs.push(fs::read_to_string(f.0.join("scripts/01_example.py")).unwrap());
    }
    assert_eq!(outputs[0], outputs[1]);
    assert_eq!(
        outputs[0],
        format!("\"\"\"Example\n\nDescription\n\"\"\"\n\n# %% Cell 1: Code\n{body}\n")
    );
    let mut notebooks = Vec::new();
    for contents in [body.to_owned(), body.replace('\n', "\r\n")] {
        let f = Fixture::new();
        let template = f.write("template.py", template_text);
        let cell = f.write("cell.py", &contents);
        let arg = format!("1={}", cell.display());
        assert!(f
            .render(&template, "notebook", &["--cell", &arg])
            .status
            .success());
        let bytes = fs::read(f.0.join("notebooks/example.ipynb")).unwrap();
        let notebook = toolkit_core::parse_json(std::str::from_utf8(&bytes).unwrap()).unwrap();
        let toolkit_core::Json::Array(cells) = notebook.get("cells").unwrap() else {
            panic!("cells")
        };
        let toolkit_core::Json::Array(source) = cells[1].get("source").unwrap() else {
            panic!("source")
        };
        assert_eq!(
            source
                .iter()
                .map(|s| s.as_str().unwrap())
                .collect::<String>(),
            body
        );
        notebooks.push(bytes);
    }
    assert_eq!(notebooks[0], notebooks[1]);
    let golden = r#"{
  "cells": [
    {
      "cell_type": "markdown",
      "metadata": {},
      "source": [
        "Description"
      ]
    },
    {
      "cell_type": "code",
      "execution_count": null,
      "metadata": {},
      "outputs": [],
      "source": [
        "def transform_dataset(df):\n",
        "    x = 1\n",
        "\n",
        "\treturn df # comment"
      ]
    }
  ],
  "metadata": {
    "kernelspec": {
      "display_name": "Python 3",
      "language": "python",
      "name": "python3"
    },
    "language_info": {
      "name": "python"
    }
  },
  "nbformat": 4,
  "nbformat_minor": 4
}
"#;
    assert_eq!(notebooks[0], golden.as_bytes());
    for ch in [
        '\r', '\0', '\x1b', '\x7f', '\u{85}', '\u{2028}', '\u{202e}', '\u{2066}', '\u{feff}',
    ] {
        let f = Fixture::new();
        let template = f.write("template.py", template_text);
        let cell = f.write("cell.py", &format!("x{ch}y"));
        let arg = format!("1={}", cell.display());
        let result = f.render(&template, "script", &["--cell", &arg]);
        assert_eq!(result.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&result.stderr).contains("source_char"));
        assert!(!f.0.join("scripts").exists());
    }
}

#[test]
fn ap_app_02_nb_08_10_12_versioned_append_and_invalid_ids() {
    use toolkit_core::{parse_json, Json};
    let f = Fixture::new();
    let template = f.write(
        "template.py",
        "# Cell 0 [markdown]: Example\n# Desc\n# Cell 1: Code\npass\n",
    );
    let base = r#"{"cells":[{"cell_type":"code","id":"a1","source":["x"]}],"metadata":{"keep":true},"nbformat":4,"nbformat_minor":5}"#;
    let source = f.write("notebooks/example-03.ipynb", base);
    f.write("notebooks/example-01.ipynb", base);
    f.write("notebooks/example-001.ipynb", "ignored");
    let result = f.render(&template, "notebook", &["--append"]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(fs::read_to_string(source).unwrap(), base);
    let copy =
        parse_json(&fs::read_to_string(f.0.join("notebooks/example-04.ipynb")).unwrap()).unwrap();
    let original = parse_json(base).unwrap();
    assert_eq!(copy.get("metadata"), original.get("metadata"));
    let Json::Array(original_cells) = original.get("cells").unwrap() else {
        panic!("cells")
    };
    assert_eq!(copy.get("nbformat_minor"), Some(&Json::Int(5)));
    let Json::Array(cells) = copy.get("cells").unwrap() else {
        panic!("cells")
    };
    assert_eq!(&cells[..original_cells.len()], original_cells.as_slice());
    assert_eq!(cells[0].get("id").unwrap().as_str(), Some("a1"));
    for cell in &cells[1..] {
        assert_eq!(cell.get("id").unwrap().as_str().unwrap().len(), 8);
    }
    let bad = Fixture::new();
    let t = bad.write(
        "template.py",
        "# Cell 0 [markdown]: Example\n# Desc\n# Cell 1: Code\npass\n",
    );
    let bad_source = bad.write(
        "notebooks/example.ipynb",
        &base.replace("\"a1\"", "\"bad id!\""),
    );
    let before = fs::read(&bad_source).unwrap();
    let result = bad.render(&t, "notebook", &["--append"]);
    assert_eq!(result.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&result.stderr).contains("notebook_cell_ids_invalid"));
    assert_eq!(fs::read(bad_source).unwrap(), before);
    assert!(!bad.0.join("notebooks/example-01.ipynb").exists());
}

#[test]
fn ap_t28_01_02_03_04_cat_11_module_contract() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/core-skills/aws-ai-ml/references/dataset-transformation/code_templates/transformation.py");
    let original = fs::read_to_string(&root).unwrap();
    assert!(original.contains("transform_fn.py"));
    assert!(original.contains("from transform_fn import transform_dataset"));
    for mode in ["script", "notebook"] {
        let f = Fixture::new();
        let missing = f.render(&root, mode, &[]);
        assert_eq!(missing.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&missing.stderr).contains("module_cell_required"));
        let module = f.write(
            "scripts/transform_fn.py",
            "def transform_dataset(df):\n    return df\n",
        );
        let before = fs::read(&module).unwrap();
        #[cfg(unix)]
        let before_meta = fs::metadata(&module).unwrap();
        let arg = format!("2={}", module.display());
        let result = f.render(&root, mode, &["--cell", &arg, "--allow-unresolved"]);
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(fs::read(&module).unwrap(), before);
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let after = fs::metadata(&module).unwrap();
            assert_eq!(
                (
                    before_meta.dev(),
                    before_meta.ino(),
                    before_meta.mtime(),
                    before_meta.mtime_nsec()
                ),
                (after.dev(), after.ino(), after.mtime(), after.mtime_nsec())
            );
        }
        let output = fs::read_to_string(f.0.join(if mode == "script" {
            "scripts/01_example.py"
        } else {
            "notebooks/example.ipynb"
        }))
        .unwrap();
        assert!(output.contains("from transform_fn import transform_dataset"));
        if mode == "script" {
            assert!(!output.contains("%%writefile"));
            assert!(output.contains("# transform_dataset is defined"));
        } else {
            let notebook = toolkit_core::parse_json(&output).unwrap();
            let toolkit_core::Json::Array(cells) = notebook.get("cells").unwrap() else {
                panic!("cells")
            };
            let toolkit_core::Json::Array(source) = cells[2].get("source").unwrap() else {
                panic!("source")
            };
            assert_eq!(
                source[0].as_str(),
                Some(
                    format!(
                        "%%writefile {}\n",
                        fs::canonicalize(&module).unwrap().display()
                    )
                    .as_str()
                )
            );
            let toolkit_core::Json::Array(source) = cells[3].get("source").unwrap() else {
                panic!("source")
            };
            let actual: String = source.iter().map(|line| line.as_str().unwrap()).collect();
            let expected = original
                .split("# Cell 3: Load Dataset\n")
                .nth(1)
                .unwrap()
                .split("\n# Cell 4:")
                .next()
                .unwrap()
                .trim_matches('\n');
            assert_eq!(actual, expected);
            assert!(output.contains("def transform_dataset(df):"));
        }
    }
}

#[test]
fn ap_con_05_template_consent_literal_must_be_false() {
    let f = Fixture::new();
    let template = f.write(
        "template.py",
        "# Cell 0 [markdown]: Example\n# Description\n# Cell 1: Code\nACCEPT_EULA = True\n",
    );
    let result = f.render(&template, "script", &[]);
    assert_eq!(result.status.code(), Some(1));
    assert!(!f.0.join("scripts").exists());
    let duplicate = f.write("duplicate.py", "# Cell 0 [markdown]: Example\n# Description\n# Cell 1: Code\nACCEPT_EULA = False\nACCEPT_EULA = True\n");
    let result = f.render(&duplicate, "script", &[]);
    assert_eq!(result.status.code(), Some(1));
    assert!(!f.0.join("scripts").exists());
}

#[test]
fn ap_cell_01_replacement_markers_remain_cell_text() {
    let f = Fixture::new();
    let template = f.write("template.py", "# Cell 0 [markdown]: Example\n# Description\n# Cell 1: Code\npass\n# Cell 2: Next\nother()\n");
    let cell = f.write("cell.py", "# Cell 99: Just a comment\nprint('ok')\n");
    let arg = format!("1={}", cell.display());
    let result = f.render(&template, "notebook", &["--cell", &arg]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let result =
        toolkit_core::parse_json(&fs::read_to_string(f.0.join("notebooks/example.ipynb")).unwrap())
            .unwrap();
    let toolkit_core::Json::Array(cells) = result.get("cells").unwrap() else {
        panic!("cells")
    };
    assert_eq!(cells.len(), 3);
    assert!(cells[1]
        .to_compact_string()
        .contains("# Cell 99: Just a comment"));
}

#[test]
fn ap_asn_01_04_05_06_09_10_assignment_contracts() {
    let source = "# Cell 0 [markdown]: Example\n# Desc\n# Cell 1: Config\nBASE_MODEL = 'old' # keep\nWAIT_FOR_COMPLETION = False\nINSTANCE_COUNT = 1\nTHRESHOLD = 0.5\nAPI_TOKEN = ''\nINPUT_LOCATION = '[INPUT_LOCATION]'\n";
    let f = Fixture::new();
    let template = f.write("template.py", source);
    let values = f.write("values.json", r#"{"values":{"INPUT_LOCATION":"s3://ok"},"assign":{"BASE_MODEL":"new","WAIT_FOR_COMPLETION":true,"INSTANCE_COUNT":2,"THRESHOLD":1.25,"API_TOKEN":null}}"#);
    let result = f.render(&template, "script", &["--values", values.to_str().unwrap()]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let output = fs::read_to_string(f.0.join("scripts/01_example.py")).unwrap();
    for text in [
        "BASE_MODEL = 'new' # keep",
        "WAIT_FOR_COMPLETION = True",
        "INSTANCE_COUNT = 2",
        "THRESHOLD = 1.25",
        "API_TOKEN = None",
        "INPUT_LOCATION = 's3://ok'",
    ] {
        assert!(output.contains(text), "{text}");
    }
    for (document, error) in [
        (r#"{"assign":{"BASE_MODEL":1}}"#, "string_value_invalid"),
        (
            r#"{"assign":{"WAIT_FOR_COMPLETION":"False"}}"#,
            "assign_value_invalid",
        ),
        (
            r#"{"assign":{"INSTANCE_COUNT":-1}}"#,
            "assign_value_invalid",
        ),
        (
            r#"{"assign":{"INPUT_LOCATION":"x"}}"#,
            "assign_target_invalid",
        ),
        (
            r#"{"assign":{"BASE_MODEL":"x","BASE_MODEL":"y"}}"#,
            "values_invalid",
        ),
        (
            r#"{"assign":{"API_TOKEN":"literal-secret-sentinel"}}"#,
            "secret_value_invalid",
        ),
    ] {
        let f = Fixture::new();
        let template = f.write("template.py", source);
        let values = f.write("values.json", document);
        let result = f.render(
            &template,
            "script",
            &["--values", values.to_str().unwrap(), "--allow-unresolved"],
        );
        assert_eq!(result.status.code(), Some(1));
        let stderr = String::from_utf8_lossy(&result.stderr);
        assert!(stderr.contains(error), "{stderr}");
        if document.contains("INPUT_LOCATION") {
            assert!(stderr.contains("values.INPUT_LOCATION"));
        }
        assert!(!stderr.contains("literal-secret-sentinel"));
        assert!(!f.0.join("scripts").exists());
    }
}

#[test]
fn ap_sub_15_20_21_sec_01_03_unknown_missing_and_secret_do_not_publish() {
    let text = "# Cell 0 [markdown]: Example\n# Desc\n# Cell 1: Config\ntoken = '[HF_TOKEN_OR_NONE]'\nif token == '[HF_TOKEN_OR_NONE]': pass\nx = '[ROLE_ARN]'\n";
    for (json, error) in [
        (
            r#"{"values":{"HF_TOKEN_OR_NONE":"literal-secret-sentinel"}}"#,
            "secret_value_invalid",
        ),
        (r#"{"values":{"TYPO":"x"}}"#, "unknown_value"),
    ] {
        let f = Fixture::new();
        let template = f.write("template.py", text);
        let values = f.write("values.json", json);
        let result = f.render(
            &template,
            "script",
            &["--values", values.to_str().unwrap(), "--allow-unresolved"],
        );
        assert_eq!(result.status.code(), Some(1));
        let stderr = String::from_utf8_lossy(&result.stderr);
        assert!(stderr.contains(error));
        assert!(!stderr.contains("literal-secret-sentinel"));
        assert!(!f.0.join("scripts").exists());
    }
    let f = Fixture::new();
    let template = f.write("template.py", text);
    let result = f.render(&template, "script", &[]);
    assert_eq!(result.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&result.stderr).contains("HF_TOKEN_OR_NONE, ROLE_ARN"));
    let values = f.write("values.json", r#"{"values":{"HF_TOKEN_OR_NONE":null}}"#);
    assert!(f
        .render(
            &template,
            "script",
            &["--values", values.to_str().unwrap(), "--allow-unresolved"]
        )
        .status
        .success());
    let output = fs::read_to_string(f.0.join("scripts/01_example.py")).unwrap();
    assert!(output.contains("token = None"));
    assert!(output.contains("token == '[HF_TOKEN_OR_NONE]'"));
    assert!(output.contains("x = '[ROLE_ARN]'"));
}

#[test]
fn ap_fs_01_04_cell_02_usage_and_input_size() {
    let f = Fixture::new();
    let template = f.write(
        "template.py",
        "# Cell 0 [markdown]: Example\n# Desc\n# Cell 1: Code\npass\n",
    );
    for name in ["", "Upper", "../x", "a/b", "example-01"] {
        let result = Command::new(env!("CARGO_BIN_EXE_toolkit"))
            .args(["ai-payloads", "render"])
            .arg(&template)
            .args(["--mode", "script", "--project-dir"])
            .arg(&f.0)
            .args(["--name", name])
            .output()
            .unwrap();
        assert!(!result.status.success(), "{name}");
        assert!(!f.0.join("scripts").exists());
    }
    for arg in ["0=x", "2=x", "bad"] {
        assert_eq!(
            f.render(&template, "script", &["--cell", arg])
                .status
                .code(),
            Some(2)
        );
    }
    let large = f.write("large.py", &"x".repeat(1024 * 1024 + 1));
    let arg = format!("1={}", large.display());
    assert_eq!(
        f.render(&template, "script", &["--cell", &arg])
            .status
            .code(),
        Some(1)
    );
    assert!(!f.0.join("scripts").exists());
}

#[cfg(unix)]
#[test]
fn ap_fs_02_03_symlink_inputs_and_output_directory_are_refused() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    let template = f.write(
        "template.py",
        "# Cell 0 [markdown]: Example\n# Desc\n# Cell 1: Code\npass\n",
    );
    let link = f.0.join("link.py");
    symlink(&template, &link).unwrap();
    assert_eq!(f.render(&link, "script", &[]).status.code(), Some(1));
    let cell_arg = format!("1={}", link.display());
    assert_eq!(
        f.render(&template, "script", &["--cell", &cell_arg])
            .status
            .code(),
        Some(1)
    );
    let values = f.write("values.json", "{}");
    let values_link = f.0.join("values-link.json");
    symlink(values, &values_link).unwrap();
    assert_eq!(
        f.render(
            &template,
            "script",
            &["--values", values_link.to_str().unwrap()]
        )
        .status
        .code(),
        Some(1)
    );
    let outside = f.0.join("outside");
    fs::create_dir(&outside).unwrap();
    symlink(&outside, f.0.join("scripts")).unwrap();
    assert_eq!(f.render(&template, "script", &[]).status.code(), Some(1));
    assert_eq!(fs::read_dir(outside).unwrap().count(), 0);
}

#[test]
fn ap_con_03_04_all_bare_slots_are_required_even_when_unresolved_allowed() {
    let names = [
        "ACCEPT_EULA",
        "TRUE_OR_FALSE",
        "CONCURRENCY",
        "REQUEST_COUNT",
        "INPUT_TOKENS_MEAN",
        "INPUT_TOKENS_STDDEV",
        "OUTPUT_TOKENS_MEAN",
        "OUTPUT_TOKENS_STDDEV",
        "METRICS_LIST",
    ];
    for name in names {
        let f = Fixture::new();
        let template = f.write(
            "template.py",
            &format!("# Cell 0 [markdown]: Example\n# Desc\n# Cell 1: Code\nx = [{name}]\n"),
        );
        let result = f.render(&template, "script", &["--allow-unresolved"]);
        assert_eq!(result.status.code(), Some(1), "{name}");
        let stderr = String::from_utf8_lossy(&result.stderr);
        assert!(stderr.contains("typed_slot_unresolved") && stderr.contains(name));
        assert!(!f.0.join("scripts").exists());
    }
    let f = Fixture::new();
    let template = f.write("template.py", TEMPLATE);
    let values = f.write("values.json", r#"{"values":{"ACCEPT_EULA":false}}"#);
    let result = f.render(
        &template,
        "script",
        &["--values", values.to_str().unwrap(), "--allow-unresolved"],
    );
    assert!(result.status.success());
    let output = fs::read_to_string(f.0.join("scripts/01_example.py")).unwrap();
    assert!(output.contains("ACCEPT_EULA = False"));
    assert!(output.contains("\"[ROLE_ARN]\""));
}

#[test]
fn ap_nb_10_11_12_unsupported_versions_and_invalid_ids() {
    for cells in [r"[{}]", r#"[{"id":"x"},{"id":"x"}]"#] {
        let f = Fixture::new();
        let template = f.write(
            "template.py",
            "# Cell 0 [markdown]: Example\n# Desc\n# Cell 1: Code\npass\n",
        );
        let text =
            format!(r#"{{"cells":{cells},"metadata":{{}},"nbformat":4,"nbformat_minor":5}}"#);
        let source = f.write("notebooks/example.ipynb", &text);
        let result = f.render(&template, "notebook", &["--append"]);
        assert_eq!(result.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&result.stderr).contains("notebook_cell_ids_invalid"));
        assert_eq!(fs::read(source).unwrap(), text.as_bytes());
        assert!(!f.0.join("notebooks/example-01.ipynb").exists());
    }
    for (version, minor, id, error) in [
        ("4", "5", "", "notebook_cell_ids_invalid"),
        ("4", "5", "bad id!", "notebook_cell_ids_invalid"),
        (
            "4",
            "5",
            "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789___",
            "notebook_cell_ids_invalid",
        ),
        ("4", "6", "a1", "notebook_version_unsupported"),
        ("4", "-1", "a1", "notebook_version_unsupported"),
        ("4", "\"5\"", "a1", "notebook_version_unsupported"),
        ("3", "4", "a1", "notebook_version_unsupported"),
        ("5", "4", "a1", "notebook_version_unsupported"),
    ] {
        let f = Fixture::new();
        let template = f.write(
            "template.py",
            "# Cell 0 [markdown]: Example\n# Desc\n# Cell 1: Code\npass\n",
        );
        let source = f.write("notebooks/example.ipynb", &format!("{{\"cells\":[{{\"id\":\"{id}\"}}],\"metadata\":{{}},\"nbformat\":{version},\"nbformat_minor\":{minor}}}"));
        let before = fs::read(&source).unwrap();
        let result = f.render(&template, "notebook", &["--append"]);
        assert_eq!(result.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&result.stderr).contains(error));
        assert_eq!(fs::read(source).unwrap(), before);
        assert!(!f.0.join("notebooks/example-01.ipynb").exists());
    }
    for minor in [0, 3, 4] {
        let f = Fixture::new();
        let template = f.write(
            "template.py",
            "# Cell 0 [markdown]: Example\n# Desc\n# Cell 1: Code\npass\n",
        );
        f.write("notebooks/example.ipynb", &format!("{{\"cells\":[{{\"source\":[]}}],\"metadata\":{{}},\"nbformat\":4,\"nbformat_minor\":{minor}}}"));
        assert!(f
            .render(&template, "notebook", &["--append"])
            .status
            .success());
        let copy = toolkit_core::parse_json(
            &fs::read_to_string(f.0.join("notebooks/example-01.ipynb")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            copy.get("nbformat_minor"),
            Some(&toolkit_core::Json::Int(minor))
        );
        let toolkit_core::Json::Array(cells) = copy.get("cells").unwrap() else {
            panic!()
        };
        assert!(cells.iter().all(|c| c.get("id").is_none()));
    }
}

#[test]
fn ap_app_02_07_series_invalid_and_exhausted_sources_are_untouched() {
    let template_text = "# Cell 0 [markdown]: Example\n# Desc\n# Cell 1: Code\npass\n";
    let f = Fixture::new();
    let template = f.write("template.py", template_text);
    let result = f.render(&template, "notebook", &["--append"]);
    assert!(String::from_utf8_lossy(&result.stderr).contains("append_source_missing"));
    let base = "{\"cells\":[],\"metadata\":{},\"nbformat\":4,\"nbformat_minor\":4}";
    let source = f.write("notebooks/example.ipynb", base);
    fs::create_dir(f.0.join("notebooks/example-05.ipynb")).unwrap();
    let result = f.render(&template, "notebook", &["--append"]);
    assert!(String::from_utf8_lossy(&result.stderr).contains("append_source_invalid"));
    assert_eq!(fs::read_to_string(source).unwrap(), base);
    let f = Fixture::new();
    let template = f.write("template.py", template_text);
    let source = f.write("notebooks/example-99.ipynb", base);
    let result = f.render(&template, "notebook", &["--append"]);
    assert!(String::from_utf8_lossy(&result.stderr).contains("notebook_versions_exhausted"));
    assert_eq!(fs::read_to_string(source).unwrap(), base);
}

#[test]
fn ap_typ_05_con_02_unused_consent_and_existing_kernel_fail_closed() {
    let f = Fixture::new();
    let template = f.write("template.py", TEMPLATE);
    for name in ["ROLE_ARN", "NOT_USED"] {
        let result = f.render(
            &template,
            "script",
            &["--consent", name, "--allow-unresolved"],
        );
        assert_eq!(result.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&result.stderr).contains("consent_name_invalid"));
    }
    let source = f.write("notebooks/example.ipynb", "{\"cells\":[{\"source\":[\"ACCEPT_EULA = False\"]}],\"metadata\":{},\"nbformat\":4,\"nbformat_minor\":4}");
    let before = fs::read(&source).unwrap();
    let result = f.render(&template, "notebook", &["--append", "--allow-unresolved"]);
    assert_eq!(result.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&result.stderr).contains("typed_slot_unresolved"));
    assert_eq!(fs::read(source).unwrap(), before);
    assert!(!f.0.join("notebooks/example-01.ipynb").exists());
}

#[test]
fn ap_t28_03_05_module_mismatch_missing_definition_and_unsafe_project() {
    let template = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/core-skills/aws-ai-ml/references/dataset-transformation/code_templates/transformation.py");
    let f = Fixture::new();
    let wrong = f.write(
        "elsewhere.py",
        "def transform_dataset(df):\n    return df\n",
    );
    let arg = format!("2={}", wrong.display());
    let result = f.render(&template, "script", &["--cell", &arg]);
    assert!(String::from_utf8_lossy(&result.stderr).contains("module_path_mismatch"));
    let module = f.write("scripts/transform_fn.py", "pass\n");
    let arg = format!("2={}", module.display());
    let result = f.render(&template, "script", &["--cell", &arg]);
    assert!(String::from_utf8_lossy(&result.stderr).contains("module_definition_missing"));
    let unsafe_dir = f.0.join("space dir");
    fs::create_dir(&unsafe_dir).unwrap();
    fs::create_dir(unsafe_dir.join("scripts")).unwrap();
    let module = unsafe_dir.join("scripts/transform_fn.py");
    fs::write(&module, "def transform_dataset(df):\n    return df\n").unwrap();
    let arg = format!("2={}", module.display());
    let result = Command::new(env!("CARGO_BIN_EXE_toolkit"))
        .args(["ai-payloads", "render"])
        .arg(&template)
        .args(["--mode", "notebook", "--project-dir"])
        .arg(&unsafe_dir)
        .args(["--name", "example", "--cell", &arg, "--allow-unresolved"])
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&result.stderr).contains("writefile_path_unsafe"));
    assert!(!unsafe_dir.join("notebooks").exists());
}

#[test]
fn ap_t28_05_magic_unsafe_project_characters() {
    let template = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/core-skills/aws-ai-ml/references/dataset-transformation/code_templates/transformation.py");
    for suffix in ["space dir", "dollar$dir", "brace{dir", "quote'dir"] {
        let f = Fixture::new();
        let project = f.0.join(suffix);
        fs::create_dir_all(project.join("scripts")).unwrap();
        let module = project.join("scripts/transform_fn.py");
        fs::write(&module, "def transform_dataset(df):\n    return df\n").unwrap();
        let cell = format!("2={}", module.display());
        let result = Command::new(env!("CARGO_BIN_EXE_toolkit"))
            .args(["ai-payloads", "render"])
            .arg(&template)
            .args(["--mode", "notebook", "--project-dir"])
            .arg(&project)
            .args(["--name", "example", "--cell", &cell, "--allow-unresolved"])
            .output()
            .unwrap();
        assert_eq!(result.status.code(), Some(1), "{suffix}");
        assert!(String::from_utf8_lossy(&result.stderr).contains("writefile_path_unsafe"));
        assert!(!project.join("notebooks").exists());
    }
}

#[test]
fn ap_nb_01_02_03_04_and_scr_01_02_output_shape() {
    use toolkit_core::{parse_json, Json};
    let source = "# Cell 0 [markdown]: Example\n# Heading\n#\n# Text\n\n# Cell 1: Work\nx = 1\n\nshow() # NOTEBOOK_ONLY\nlast()\n";
    let f = Fixture::new();
    let template = f.write("template.py", source);
    let result = f.render(&template, "notebook", &[]);
    assert!(result.status.success());
    let text = fs::read_to_string(f.0.join("notebooks/example.ipynb")).unwrap();
    let golden = r#"{
  "cells": [
    {
      "cell_type": "markdown",
      "metadata": {},
      "source": [
        "Heading\n",
        "\n",
        "Text"
      ]
    },
    {
      "cell_type": "code",
      "execution_count": null,
      "metadata": {},
      "outputs": [],
      "source": [
        "x = 1\n",
        "\n",
        "show() # NOTEBOOK_ONLY\n",
        "last()"
      ]
    }
  ],
  "metadata": {
    "kernelspec": {
      "display_name": "Python 3",
      "language": "python",
      "name": "python3"
    },
    "language_info": {
      "name": "python"
    }
  },
  "nbformat": 4,
  "nbformat_minor": 4
}
"#;
    assert_eq!(text, golden);
    let notebook = parse_json(&text).unwrap();
    assert_eq!(notebook.get("nbformat"), Some(&Json::Int(4)));
    assert_eq!(notebook.get("nbformat_minor"), Some(&Json::Int(4)));
    let Json::Array(cells) = notebook.get("cells").unwrap() else {
        panic!()
    };
    assert_eq!(cells.len(), 2);
    assert_eq!(
        cells[0].get("cell_type").unwrap().as_str(),
        Some("markdown")
    );
    assert!(cells[0].get("outputs").is_none());
    assert_eq!(
        cells[0].get("source"),
        Some(&Json::Array(vec![
            Json::string("Heading\n"),
            Json::string("\n"),
            Json::string("Text")
        ]))
    );
    assert_eq!(
        cells[1].get("source"),
        Some(&Json::Array(vec![
            Json::string("x = 1\n"),
            Json::string("\n"),
            Json::string("show() # NOTEBOOK_ONLY\n"),
            Json::string("last()")
        ]))
    );
    assert_eq!(cells[1].get("execution_count"), Some(&Json::Null));
    assert_eq!(cells[1].get("outputs"), Some(&Json::Array(vec![])));
    assert!(f.render(&template, "script", &[]).status.success());
    assert_eq!(
        fs::read_to_string(f.0.join("scripts/01_example.py")).unwrap(),
        "\"\"\"Example\n\nHeading\n\nText\n\"\"\"\n\n# %% Cell 1: Work\nx = 1\n\nlast()\n"
    );
}

#[test]
fn ap_sub_11_12_invalid_scalars_never_echo_values() {
    let f = Fixture::new();
    let template = f.write(
        "template.py",
        "# Cell 0 [markdown]: Example\n# Desc\n# Cell 1: Code\nx = '[ROLE_ARN]'\n",
    );
    for value in [
        toolkit_core::Json::string("sensitive-value\nnext"),
        toolkit_core::Json::string("\u{202e}"),
        toolkit_core::Json::string("x".repeat(4097)),
        toolkit_core::Json::Bool(true),
        toolkit_core::Json::Int(12),
    ] {
        let document = toolkit_core::Json::object(vec![(
            "values",
            toolkit_core::Json::object(vec![("ROLE_ARN", value)]),
        )]);
        let file = f.write("values.json", &document.to_compact_string());
        let result = f.render(&template, "script", &["--values", file.to_str().unwrap()]);
        assert_eq!(result.status.code(), Some(1));
        let stderr = String::from_utf8_lossy(&result.stderr);
        assert!(stderr.contains("ROLE_ARN"));
        assert!(!stderr.contains("sensitive-value"));
        assert!(!f.0.join("scripts").exists());
    }
}

#[test]
fn ap_asn_05_type_errors_across_values_and_assign_are_collected_sorted() {
    let f = Fixture::new();
    let template = f.write("template.py", "# Cell 0 [markdown]: Example\n# Desc\n# Cell 1: Code\nA_FIELD = 1\nZ_FIELD = False\nx = '[MIDDLE]'\n");
    let values = f.write(
        "values.json",
        r#"{"values":{"MIDDLE":false},"assign":{"Z_FIELD":"bad","A_FIELD":-1}}"#,
    );
    let result = f.render(&template, "script", &["--values", values.to_str().unwrap()]);
    assert_eq!(result.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&result.stderr);
    let a = stderr.find("A_FIELD").unwrap();
    let middle = stderr.find("MIDDLE").unwrap();
    let z = stderr.find("Z_FIELD").unwrap();
    assert!(a < middle && middle < z, "{stderr}");
    assert!(!f.0.join("scripts").exists());
}

#[test]
fn ap_nb_06_existing_notebook_is_never_overwritten() {
    let f = Fixture::new();
    let template = f.write(
        "template.py",
        "# Cell 0 [markdown]: Example\n# Desc\n# Cell 1: Code\npass\n",
    );
    let destination = f.write("notebooks/example.ipynb", "existing bytes");
    for flags in [&[][..], &["--new"][..]] {
        let result = f.render(&template, "notebook", flags);
        assert_eq!(result.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&result.stderr).contains("destination_exists"));
        assert_eq!(fs::read_to_string(&destination).unwrap(), "existing bytes");
    }
    assert!(fs::read_dir(f.0.join("notebooks"))
        .unwrap()
        .all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains("toolkit-render")));
}

#[test]
fn ap_app_09_production_sources_do_not_modify_existing_files() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/ai_payloads");
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|x| x.to_str()) != Some("rs") {
            continue;
        }
        let source = fs::read_to_string(&path).unwrap();
        for forbidden in [
            "std::fs::rename",
            "fs::rename(",
            ".set_len(",
            ".try_lock(",
            ".lock(",
        ] {
            assert!(
                !source.contains(forbidden),
                "{}: {forbidden}",
                path.display()
            );
        }
        if path.file_name().unwrap() != "place.rs" {
            assert!(!source.contains("OpenOptions::new()"), "{}", path.display());
        }
    }
}

#[test]
fn ap_asn_04_07_08_invalid_targets_and_replaced_cells() {
    let source = "# Cell 0 [markdown]: Example\n# Desc\n# Cell 1: Config\nVALUE = 1\nDUP = 1\nDUP = 2\na, TUPLE = 1, 2\nFSTRING = f'no'\nNONE = None\nEXPR = other + 1\n";
    for name in ["DUP", "TUPLE", "FSTRING", "NONE", "EXPR", "UNKNOWN"] {
        let f = Fixture::new();
        let template = f.write("template.py", source);
        let values = f.write("values.json", &format!("{{\"assign\":{{\"{name}\":1}}}}"));
        let result = f.render(&template, "script", &["--values", values.to_str().unwrap()]);
        assert_eq!(result.status.code(), Some(1));
        assert!(
            String::from_utf8_lossy(&result.stderr).contains("assign_target_invalid"),
            "{name}"
        );
    }
    let f = Fixture::new();
    let template = f.write("template.py", source);
    let values = f.write("values.json", r#"{"assign":{"VALUE":2}}"#);
    let cell = f.write("cell.py", "pass\n");
    let arg = format!("1={}", cell.display());
    let result = f.render(
        &template,
        "script",
        &["--values", values.to_str().unwrap(), "--cell", &arg],
    );
    assert!(String::from_utf8_lossy(&result.stderr).contains("assign_target_replaced"));
    let result = f.render(&template, "script", &["--cell", &arg, "--cell", &arg]);
    assert_eq!(result.status.code(), Some(2));
}

#[test]
fn ap_sub_16_sec_04_asn_03_project_default_and_section_three_assignment() {
    let f = Fixture::new();
    let template = f.write("template.py", "# Cell 0 [markdown]: Example\n# Desc\n# Cell 1: Config\nx = '[PROJECT_DIR]'\nINPUT_TOKENS_MEAN = [INPUT_TOKENS_MEAN]\n# Cell 2: Other\npass\n# Cell 3: Custom\nCUSTOM_PROMPT_PATH = 'old'\n");
    let values = f.write(
        "values.json",
        r#"{"values":{"INPUT_TOKENS_MEAN":1},"assign":{"CUSTOM_PROMPT_PATH":"new"}}"#,
    );
    let result = f.render(&template, "script", &["--values", values.to_str().unwrap()]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let output = fs::read_to_string(f.0.join("scripts/01_example.py")).unwrap();
    assert!(output.contains(&format!(
        "x = '{}'",
        fs::canonicalize(&f.0).unwrap().display()
    )));
    assert!(output.contains("INPUT_TOKENS_MEAN = 1"));
    assert!(output.contains("CUSTOM_PROMPT_PATH = 'new'"));
}

#[cfg(unix)]
#[test]
fn ap_t28_03_symlinked_module_is_refused() {
    use std::os::unix::fs::symlink;
    let template = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/core-skills/aws-ai-ml/references/dataset-transformation/code_templates/transformation.py");
    let f = Fixture::new();
    let target = f.write("target.py", "def transform_dataset(df):\n    return df\n");
    fs::create_dir(f.0.join("scripts")).unwrap();
    let module = f.0.join("scripts/transform_fn.py");
    symlink(target, &module).unwrap();
    let arg = format!("2={}", module.display());
    let result = f.render(&template, "notebook", &["--cell", &arg]);
    assert_eq!(result.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&result.stderr).contains("module_missing"));
    assert!(!f.0.join("notebooks").exists());
    fs::remove_file(module).unwrap();
    let module = f.write(
        "scripts/transform_fn.py",
        "def transform_dataset(df):\n    return df\n",
    );
    let alias = f.0.join("alias");
    symlink(f.0.join("scripts"), &alias).unwrap();
    let arg = format!("2={}", alias.join("transform_fn.py").display());
    let result = f.render(&template, "notebook", &["--cell", &arg]);
    assert_eq!(result.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&result.stderr).contains("module_missing"));
    assert!(module.exists());
}

#[cfg(unix)]
#[test]
fn ap_app_01_08_source_identity_and_readonly_mode_are_preserved() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let f = Fixture::new();
    let template = f.write(
        "template.py",
        "# Cell 0 [markdown]: Example\n# Desc\n# Cell 1: Code\npass\n",
    );
    let source = f.write(
        "notebooks/example.ipynb",
        "{\"cells\":[],\"metadata\":{},\"nbformat\":4,\"nbformat_minor\":4}",
    );
    fs::set_permissions(&source, fs::Permissions::from_mode(0o444)).unwrap();
    let before = fs::metadata(&source).unwrap();
    let bytes = fs::read(&source).unwrap();
    let result = f.render(&template, "notebook", &["--append"]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let after = fs::metadata(&source).unwrap();
    assert_eq!(
        (
            before.dev(),
            before.ino(),
            before.len(),
            before.mtime(),
            before.mtime_nsec()
        ),
        (
            after.dev(),
            after.ino(),
            after.len(),
            after.mtime(),
            after.mtime_nsec()
        )
    );
    assert_eq!(fs::read(source).unwrap(), bytes);
}

#[test]
fn ap_asn_11_manual_edits_reported_after_success() {
    let f = Fixture::new();
    let template = f.write("template.py", "# Cell 0 [markdown]: Example\n# Desc\n# Cell 1: Code\n# NOTE FOR AGENT: change the config\npass\n");
    let result = f.render(&template, "script", &[]);
    assert!(result.status.success());
    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(
        stdout.contains("manual: Cell 1 line 4: NOTE FOR AGENT: change the config"),
        "{stdout}"
    );
}

#[test]
fn ap_asn_07_secret_sentinel_is_unchanged_with_values_and_assign() {
    let f = Fixture::new();
    let template = f.write("template.py", "# Cell 0 [markdown]: Example\n# Desc\n# Cell 1: Code\nBASE_MODEL = 'old'\nHF_HUB_TOKEN = '[HF_TOKEN_OR_NONE]'\nif HF_HUB_TOKEN in ('', 'None', '[HF_TOKEN_OR_NONE]'): pass\n");
    let values = f.write(
        "values.json",
        r#"{"values":{"HF_TOKEN_OR_NONE":null},"assign":{"BASE_MODEL":"new"}}"#,
    );
    let result = f.render(&template, "script", &["--values", values.to_str().unwrap()]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let output = fs::read_to_string(f.0.join("scripts/01_example.py")).unwrap();
    assert!(output.contains("HF_HUB_TOKEN = None"));
    assert!(output.contains("if HF_HUB_TOKEN in ('', 'None', '[HF_TOKEN_OR_NONE]'): pass"));
}

#[cfg(unix)]
#[test]
fn ap_app_02_07_latest_symlink_and_over_cap_source_are_refused() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    let template = f.write(
        "template.py",
        "# Cell 0 [markdown]: Example\n# Desc\n# Cell 1: Code\npass\n",
    );
    let base = f.write(
        "notebooks/example.ipynb",
        "{\"cells\":[],\"metadata\":{},\"nbformat\":4,\"nbformat_minor\":4}",
    );
    symlink(&base, f.0.join("notebooks/example-05.ipynb")).unwrap();
    let result = f.render(&template, "notebook", &["--append"]);
    assert!(String::from_utf8_lossy(&result.stderr).contains("append_source_invalid"));
    assert!(!f.0.join("notebooks/example-06.ipynb").exists());
    fs::remove_file(f.0.join("notebooks/example-05.ipynb")).unwrap();
    let huge = f.write(
        "notebooks/example-06.ipynb",
        &"x".repeat(16 * 1024 * 1024 + 1),
    );
    let result = f.render(&template, "notebook", &["--append"]);
    assert!(String::from_utf8_lossy(&result.stderr).contains("append_source_invalid"));
    assert_eq!(fs::read(huge).unwrap(), vec![b'x'; 16 * 1024 * 1024 + 1]);
    assert!(!f.0.join("notebooks/example-07.ipynb").exists());
    let fifo = f.0.join("notebooks/example-08.ipynb");
    assert!(Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap()
        .success());
    let result = f.render(&template, "notebook", &["--append"]);
    assert!(String::from_utf8_lossy(&result.stderr).contains("append_source_invalid"));
    assert!(!f.0.join("notebooks/example-09.ipynb").exists());
}

#[test]
fn ap_con_05_consent_slot_and_assignment_emissions() {
    for mode in ["notebook", "script"] {
        for (source, json, consent, emitted) in [
            (
                "ACCEPT_EULA = [ACCEPT_EULA]",
                r#"{"values":{"ACCEPT_EULA":false}}"#,
                false,
                "False",
            ),
            (
                "ACCEPT_EULA = [ACCEPT_EULA]",
                r#"{"values":{"ACCEPT_EULA":true}}"#,
                true,
                "True",
            ),
            (
                "ACCEPT_EULA = False",
                r#"{"assign":{"ACCEPT_EULA":false}}"#,
                false,
                "False",
            ),
            (
                "ACCEPT_EULA = False",
                r#"{"assign":{"ACCEPT_EULA":true}}"#,
                true,
                "True",
            ),
            ("ACCEPT_EULA = False", "{}", false, "False"),
        ] {
            let f = Fixture::new();
            let template = f.write(
                "template.py",
                &format!("# Cell 0 [markdown]: Example\n# Desc\n# Cell 1: Code\n{source}\n"),
            );
            let values = f.write("values.json", json);
            let mut args = vec!["--values", values.to_str().unwrap()];
            if consent {
                args.extend(["--consent", "ACCEPT_EULA"]);
            }
            let result = f.render(&template, mode, &args);
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            let output = fs::read_to_string(f.0.join(if mode == "script" {
                "scripts/01_example.py"
            } else {
                "notebooks/example.ipynb"
            }))
            .unwrap();
            assert!(output.contains(&format!("ACCEPT_EULA = {emitted}")));
            assert!(!output.contains("[ACCEPT_EULA]"));
        }
    }
}

#[test]
fn ap_asn_01_sub_10_assignment_and_slot_on_same_line() {
    let f = Fixture::new();
    let template = f.write(
        "template.py",
        "# Cell 0 [markdown]: Example\n# Desc\n# Cell 1: Code\nA = 1; x = \"[ROLE_ARN]\"\n",
    );
    let values = f.write(
        "values.json",
        r#"{"values":{"ROLE_ARN":"ok"},"assign":{"A":12345}}"#,
    );
    let result = f.render(&template, "script", &["--values", values.to_str().unwrap()]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(fs::read_to_string(f.0.join("scripts/01_example.py"))
        .unwrap()
        .contains("A = 12345; x = \"ok\""));
}

//! Offline process contracts for the S2b renderer. No payload is executed.
use std::{fs, path::{Path, PathBuf}, process::Command, sync::atomic::{AtomicUsize, Ordering}};

static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("payload-render-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
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
        cmd.args(["ai-payloads", "render"]).arg(template).args(["--mode", mode, "--project-dir"]).arg(&self.0).args(["--name", "example"]).args(extra);
        cmd.output().unwrap()
    }
}
impl Drop for Fixture { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
const TEMPLATE: &str = "# Cell 0 [markdown]: Example\n# Description\n\n# Cell 1: Config\nACCEPT_EULA = [ACCEPT_EULA]\nrole = \"[ROLE_ARN]\"\n";

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
    let values = f.write("values.json", r#"{"values":{"ACCEPT_EULA":true,"ROLE_ARN":"role"}}"#);
    let value_path = values.to_str().unwrap();
    let denied = f.render(&template, "script", &["--values", value_path]);
    assert_eq!(denied.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&denied.stderr).contains("consent_not_given"));
    assert!(!f.0.join("scripts/01_example.py").exists());
    let allowed = f.render(&template, "script", &["--values", value_path, "--consent", "ACCEPT_EULA"]);
    assert!(allowed.status.success(), "{}", String::from_utf8_lossy(&allowed.stderr));
    assert!(fs::read_to_string(f.0.join("scripts/01_example.py")).unwrap().contains("ACCEPT_EULA = True"));
}

#[test]
fn ap_app_01_historical_consent_is_preserved_and_new_false_needs_no_consent() {
    let f = Fixture::new();
    let template = f.write("template.py", TEMPLATE);
    let values = f.write("values.json", r#"{"values":{"ACCEPT_EULA":false,"ROLE_ARN":"role"}}"#);
    let source = f.write("notebooks/example.ipynb", r#"{"cells":[{"cell_type":"code","execution_count":null,"metadata":{},"outputs":[],"source":["ACCEPT_EULA = True\n","ACCEPT_EULA = historical_expression\n"]}],"metadata":{},"nbformat":4,"nbformat_minor":4}"#);
    let original = fs::read(&source).unwrap();
    let result = f.render(&template, "notebook", &["--values", values.to_str().unwrap(), "--append"]);
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
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
    let values = f.write("values.json", r#"{"values":{"ACCEPT_EULA":false,"ROLE_ARN":"role"}}"#);
    let result = f.render(&template, "notebook", &["--values", values.to_str().unwrap(), "--dry-run"]);
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    assert!(!f.0.join("notebooks/example.ipynb").exists());
}

#![cfg(unix)]

#[cfg(feature = "agents-pay-live")]
compile_error!("agents-pay-live is never built for tests (AC3)");

use std::fs;
use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use toolkit_cli::agents_pay::config::{
    bounded_env, load_config, load_config_with_hook, load_config_with_owner, require_resource,
    resolve_resource, runtime_config_path, Tunables,
};
use toolkit_core::{parse_json, Json};

static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "x402-config-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&p).unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o700)).unwrap();
        Self(p)
    }
    fn write(&self, contents: &str) -> PathBuf {
        let p = self.0.join("config.json");
        fs::write(&p, contents).unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o600)).unwrap();
        p
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o700));
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn policy_file_and_directory_protections() {
    let f = Fixture::new();
    let p = f.write(r#"{"max_amount_usd":"0.50"}"#);
    let cfg = load_config(&p).unwrap();
    assert_eq!(cfg.get("max_per_payment_usd"), Some(&Json::string("0.50")));
    for mode in [0o640, 0o604, 0o666, 0o660] {
        fs::set_permissions(&p, fs::Permissions::from_mode(mode)).unwrap();
        assert!(load_config(&p).unwrap_err().contains("expected 0600"));
    }
    fs::set_permissions(&p, fs::Permissions::from_mode(0o600)).unwrap();
    for mode in [0o777, 0o722, 0o770] {
        fs::set_permissions(&f.0, fs::Permissions::from_mode(mode)).unwrap();
        assert!(load_config(&p).unwrap_err().contains("writable"));
    }
    fs::set_permissions(&f.0, fs::Permissions::from_mode(0o700)).unwrap();
    fs::remove_file(&p).unwrap();
    assert!(load_config(&p).unwrap_err().contains("No payment config"));
    symlink("missing", &p).unwrap();
    assert!(load_config(&p).unwrap_err().contains("symlink"));
}

#[test]
fn owner_refusals_are_exact() {
    let f = Fixture::new();
    let p = f.write(r#"{"max_per_payment_usd":"1"}"#);
    let dir_owner = fs::metadata(&f.0).unwrap().uid();
    let file_owner = fs::metadata(&p).unwrap().uid();
    assert_eq!(
        load_config_with_owner(&p, || {}, || dir_owner.wrapping_add(1)).unwrap_err(),
        format!(
            "Directory {} is not owned by the current user.",
            f.0.display()
        )
    );
    let mut owners = [dir_owner, file_owner.wrapping_add(1)].into_iter();
    assert_eq!(
        load_config_with_owner(&p, || {}, || owners.next().unwrap()).unwrap_err(),
        format!(
            "Payment config {} is not owned by the current user.",
            p.display()
        )
    );
}

#[test]
fn policy_file_rejects_invalid_documents_and_caps() {
    let f = Fixture::new();
    for input in [
        "{}",
        r#"{"max_per_payment_usd":"0"}"#,
        r#"{"max_per_payment_usd":"-1"}"#,
        r#"{"max_per_payment_usd":"bad"}"#,
        "[]",
        r#"{"a":1,"a":2}"#,
    ] {
        let p = f.write(input);
        assert!(load_config(&p).is_err(), "{input}");
    }
}

#[test]
fn config_precedence_and_single_tenant_resolution() {
    let f = Fixture::new();
    let p = f.write(r#"{"policy":{"max_per_payment_usd":"1"},"resources":{"payment_session_id":"trusted","user_id":"alice","region":true}}"#);
    let cfg = load_config(&p).unwrap();
    let env = |name: &str| match name {
        "PAYMENT_SESSION_ID" => Some("hostile".to_owned()),
        "PAYMENT_USER_ID" => Some("other".to_owned()),
        _ => None,
    };
    assert_eq!(
        resolve_resource(&cfg, "payment_session_id", &env)
            .unwrap()
            .as_deref(),
        Some("trusted")
    );
    assert_eq!(
        resolve_resource(&cfg, "user_id", &env).unwrap().as_deref(),
        Some("alice")
    );
    assert_eq!(
        resolve_resource(&cfg, "region", &env).unwrap().as_deref(),
        Some("True")
    );
    let empty = parse_json(r#"{"_resources":{"user_id":[],"payment_session_id":null}}"#).unwrap();
    assert_eq!(
        resolve_resource(&empty, "payment_session_id", &env)
            .unwrap()
            .as_deref(),
        Some("hostile")
    );
    let invalid = parse_json(r#"{"_resources":{"user_id":[1]}}"#).unwrap();
    assert!(resolve_resource(&invalid, "user_id", &env).is_err());
    assert!(runtime_config_path().unwrap().is_absolute());
}

#[test]
fn t_cfg_race_and_size() {
    let f = Fixture::new();
    let p = f.write(r#"{"max_per_payment_usd":"1"}"#);
    let other = f.0.join("other");
    fs::write(&other, r#"{"max_per_payment_usd":"999"}"#).unwrap();
    fs::set_permissions(&other, fs::Permissions::from_mode(0o600)).unwrap();
    let err = load_config_with_hook(&p, || {
        fs::remove_file(&p).unwrap();
        symlink(&other, &p).unwrap();
    })
    .unwrap_err();
    assert!(err.contains("symlink"));
    fs::remove_file(&p).unwrap();
    let valid = r#"{"max_per_payment_usd":"1"}"#;
    f.write(&format!("{valid}{}", " ".repeat(1_048_576 - valid.len())));
    assert!(load_config(&p).is_ok());
    f.write(&format!("{valid}{}", " ".repeat(1_048_577 - valid.len())));
    assert!(load_config(&p).unwrap_err().contains("too large"));
}

#[test]
// These integer-valued bounds and defaults are exactly representable in f64.
#[allow(clippy::float_cmp)]
fn tunables_names_bounds_and_unicode() {
    let none = |_: &str| None;
    assert_eq!(
        Tunables::from_env(&none),
        Tunables {
            max_body_bytes: 262_144,
            timeout_seconds: 20.0,
            max_payment_attempts: 5
        }
    );
    let values = |name: &str| match name {
        "X402_MAX_BODY_BYTES" => Some("2048.9".into()),
        "X402_TIMEOUT_SECONDS" => Some("7".into()),
        "X402_MAX_PAYMENT_ATTEMPTS" => Some("5.9".into()),
        _ => None,
    };
    assert_eq!(
        Tunables::from_env(&values),
        Tunables {
            max_body_bytes: 2048,
            timeout_seconds: 7.0,
            max_payment_attempts: 5
        }
    );
    let nan = |_: &str| Some("nan".into());
    assert_eq!(Tunables::from_env(&nan), Tunables::from_env(&none));
    for (default, min, max) in [
        (262_144.0, 1024.0, 67_108_864.0),
        (20.0, 1.0, 120.0),
        (5.0, 1.0, 10.0),
    ] {
        for raw in [
            "１２３",
            "١٢٣",
            "\u{1c}1\u{1d}",
            "abc",
            "",
            "-1",
            "999999999",
            "infinity",
            "-NaN",
        ] {
            assert_eq!(
                bounded_env(Some(raw), default, min, max),
                default,
                "{raw:?}"
            );
        }
        assert_eq!(bounded_env(None, default, min, max), default);
        assert_eq!(
            bounded_env(Some(&format!("\u{a0}{min}\u{a0}")), default, min, max),
            min
        );
        assert_eq!(bounded_env(Some(&min.to_string()), default, min, max), min);
        assert_eq!(bounded_env(Some(&max.to_string()), default, min, max), max);
    }
    assert_eq!(bounded_env(Some("99999"), 5.0, 1.0, 10.0), 5.0);
}

#[test]
fn config_shapes_and_required_resources() {
    let f = Fixture::new();
    let p = f.write(r#"{"policy":false,"resources":[],"_resources":{"user_id":"hostile"},"max_per_payment_usd":"2","max_amount_usd":"3"}"#);
    let cfg = load_config(&p).unwrap();
    assert_eq!(cfg.get("max_per_payment_usd"), Some(&Json::string("2")));
    assert_eq!(cfg.get("_resources"), Some(&Json::Object(vec![])));
    let none = |_: &str| None;
    assert_eq!(resolve_resource(&cfg, "user_id", &none).unwrap(), None);
    let empty = |_: &str| Some(String::new());
    for (key, name) in [("user_id", "PAYMENT_USER_ID"), ("custom_key", "CUSTOM_KEY")] {
        let expected = format!("Missing payment resource '{key}'. Add it to the config (toolkit agents-pay admin init-config) or set {name}. Provision resources with the trusted setup path first.");
        assert_eq!(require_resource(&cfg, key, &none).unwrap_err(), expected);
        assert_eq!(require_resource(&cfg, key, &empty).unwrap_err(), expected);
    }
}

#[test]
fn subprocess_home_and_relative_path() {
    if std::env::var_os("X402_CONFIG_CHILD").is_some() {
        let expected = nix::unistd::User::from_uid(nix::unistd::Uid::current())
            .unwrap()
            .unwrap()
            .dir
            .join(".agents-pay/config.json");
        assert_eq!(runtime_config_path().unwrap(), expected);
        assert!(load_config(std::path::Path::new("config.json")).is_ok());
        return;
    }
    let f = Fixture::new();
    f.write(r#"{"max_per_payment_usd":"1"}"#);
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "subprocess_home_and_relative_path"])
        .current_dir(&f.0)
        .env("HOME", &f.0)
        .env("X402_CONFIG_CHILD", "1")
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
fn mode_and_file_type_refusals() {
    let f = Fixture::new();
    let p = f.write(r#"{"max_per_payment_usd":"1"}"#);
    for (mode, rendered) in [
        (0o4770, "-rwsrwx---"),
        (0o2660, "-rw-rwS---"),
        (0o1661, "-rw-rw---t"),
        (0o1660, "-rw-rw---T"),
    ] {
        fs::set_permissions(&p, fs::Permissions::from_mode(mode)).unwrap();
        assert!(load_config(&p).unwrap_err().contains(rendered));
    }
    fs::set_permissions(&f.0, fs::Permissions::from_mode(0o1777)).unwrap();
    assert!(load_config(&p).unwrap_err().contains("drwxrwxrwt"));
    fs::set_permissions(&f.0, fs::Permissions::from_mode(0o700)).unwrap();
    fs::remove_file(&p).unwrap();
    fs::create_dir(&p).unwrap();
    assert!(load_config(&p).unwrap_err().contains("not a regular file"));
    if rustix::process::getuid().as_raw() != 0 {
        assert!(load_config(std::path::Path::new("/etc/passwd"))
            .unwrap_err()
            .contains("not owned by the current user"));
    }
}

#[test]
fn t_tune_python_float_and_bounds() {
    for (raw, expected) in [
        ("nan", 5.0),
        ("inf", 5.0),
        ("1e1", 10.0),
        ("5.9", 5.9),
        ("  +1_0  ", 10.0),
        ("1__0", 5.0),
        ("0", 5.0),
    ] {
        assert!(
            (bounded_env(Some(raw), 5.0, 1.0, 10.0) - expected).abs() < f64::EPSILON,
            "{raw}"
        );
    }
}

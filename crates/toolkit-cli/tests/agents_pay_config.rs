#![cfg(unix)]

use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use toolkit_cli::agents_pay::config::{bounded_env, load_config, load_config_with_hook, resolve_resource, runtime_config_path};
use toolkit_core::{parse_json, Json};

static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("x402-config-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
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
impl Drop for Fixture { fn drop(&mut self) { let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o700)); let _ = fs::remove_dir_all(&self.0); } }

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
fn policy_file_rejects_invalid_documents_and_caps() {
    let f = Fixture::new();
    for input in ["{}", r#"{"max_per_payment_usd":"0"}"#, r#"{"max_per_payment_usd":"-1"}"#, r#"{"max_per_payment_usd":"bad"}"#, "[]", r#"{"a":1,"a":2}"#] {
        let p = f.write(input);
        assert!(load_config(&p).is_err(), "{input}");
    }
}

#[test]
fn config_precedence_and_single_tenant_resolution() {
    let f = Fixture::new();
    let p = f.write(r#"{"policy":{"max_per_payment_usd":"1"},"resources":{"payment_session_id":"trusted","user_id":"alice","region":true}}"#);
    let cfg = load_config(&p).unwrap();
    let env = |name: &str| match name { "PAYMENT_SESSION_ID" => Some("hostile".to_owned()), "PAYMENT_USER_ID" => Some("other".to_owned()), _ => None };
    assert_eq!(resolve_resource(&cfg, "payment_session_id", &env).unwrap().as_deref(), Some("trusted"));
    assert_eq!(resolve_resource(&cfg, "user_id", &env).unwrap().as_deref(), Some("alice"));
    assert_eq!(resolve_resource(&cfg, "region", &env).unwrap().as_deref(), Some("True"));
    let empty = parse_json(r#"{"_resources":{"user_id":[],"payment_session_id":null}}"#).unwrap();
    assert_eq!(resolve_resource(&empty, "payment_session_id", &env).unwrap().as_deref(), Some("hostile"));
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
    let err = load_config_with_hook(&p, || { fs::remove_file(&p).unwrap(); symlink(&other, &p).unwrap(); }).unwrap_err();
    assert!(err.contains("symlink"));
    fs::remove_file(&p).unwrap();
    f.write(&" ".repeat(1_048_577));
    assert!(load_config(&p).unwrap_err().contains("too large"));
}

#[test]
fn t_tune_python_float_and_bounds() {
    for (raw, expected) in [("nan", 5.0), ("inf", 5.0), ("1e1", 10.0), ("5.9", 5.9), ("  +1_0  ", 10.0), ("1__0", 5.0), ("0", 5.0)] {
        assert_eq!(bounded_env(Some(raw), 5.0, 1.0, 10.0), expected, "{raw}");
    }
}

#![cfg(feature = "sdk")]

use std::cell::RefCell;
use std::path::Path;
use toolkit_aws::sdk::roots::{LoadedCerts, RootEnv};
use toolkit_aws::sdk::roots_payment::{
    validate_payment_roots, PaymentRootLoader, PaymentRootPlatform, PaymentRootsFailure,
};

#[derive(Default)]
struct Recorder(RefCell<Vec<String>>);
impl PaymentRootLoader for Recorder {
    fn exists(&self, path: &Path) -> bool {
        self.0
            .borrow_mut()
            .push(format!("exists:{}", path.display()));
        false
    }
    fn load_paths(&self, file: Option<&Path>, dir: Option<&Path>) -> LoadedCerts {
        self.0.borrow_mut().push(format!("load:{file:?}:{dir:?}"));
        LoadedCerts::default()
    }
    fn native_store(&self) -> LoadedCerts {
        self.0.borrow_mut().push("native".into());
        LoadedCerts::default()
    }
}

#[test]
fn payment_roots_ignore_unix_environment_and_refuse_empty_store() {
    let baseline = Recorder::default();
    assert_eq!(
        validate_payment_roots(
            &RootEnv::default(),
            &baseline,
            PaymentRootPlatform::UnixFixedTable
        )
        .unwrap_err(),
        PaymentRootsFailure::NoRoots
    );
    let hostile = Recorder::default();
    let env = RootEnv {
        file: Some("/hostile".into()),
        dir: Some(":".into()),
    };
    assert_eq!(
        validate_payment_roots(&env, &hostile, PaymentRootPlatform::UnixFixedTable).unwrap_err(),
        PaymentRootsFailure::NoRoots
    );
    assert_eq!(*baseline.0.borrow(), *hostile.0.borrow());
    assert!(!hostile.0.borrow().iter().any(|entry| entry == "native"));
}

#[test]
fn payment_roots_native_store_refuses_presence_before_loading() {
    for env in [
        RootEnv {
            file: Some("".into()),
            dir: None,
        },
        RootEnv {
            file: None,
            dir: Some("".into()),
        },
    ] {
        let recorder = Recorder::default();
        assert_eq!(
            validate_payment_roots(&env, &recorder, PaymentRootPlatform::NativeStoreOnly)
                .unwrap_err(),
            PaymentRootsFailure::OverrideVariablePresent
        );
        assert!(recorder.0.borrow().is_empty());
    }
    let recorder = Recorder::default();
    assert_eq!(
        validate_payment_roots(
            &RootEnv::default(),
            &recorder,
            PaymentRootPlatform::NativeStoreOnly
        )
        .unwrap_err(),
        PaymentRootsFailure::NoRoots
    );
    assert_eq!(*recorder.0.borrow(), ["native"]);
}

#[test]
fn unix_payment_source_does_not_use_environment_sensitive_loaders() {
    let source = include_str!("../src/sdk/roots_payment.rs");
    let production = source.split("#[cfg(test)]").next().unwrap();
    for forbidden in [
        "openssl_probe::probe",
        "std::env::var",
        "set_var",
        "remove_var",
        "AWS_CA_BUNDLE",
    ] {
        assert!(!production.contains(forbidden), "forbidden: {forbidden}");
    }
    let native = production
        .find("let result = rustls_native_certs::load_native_certs()")
        .unwrap();
    let guard = production[..native]
        .rfind("#[cfg(not(all(unix, not(target_os = \"macos\"))))]")
        .unwrap();
    assert!(production[guard..native].contains("fn native_store"));
}

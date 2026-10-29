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

// Parse the file, remove only the two narrowly guarded exceptions, then inspect
// tokens recursively. Comments and strings cannot hide or create a match.
fn source_gate(source: &str) -> Result<(), String> {
    use quote::ToTokens;
    use syn::{ImplItem, Item, Stmt};
    fn scan(tokens: proc_macro2::TokenStream) -> Result<(), String> {
        use proc_macro2::TokenTree;
        let mut words = Vec::new();
        for token in tokens {
            match token {
                TokenTree::Group(g) => {
                    scan(g.stream())?;
                    words.push("group".to_owned());
                }
                TokenTree::Ident(i) => words.push(i.to_string()),
                TokenTree::Punct(p) => words.push(p.as_char().to_string()),
                TokenTree::Literal(_) => words.push("literal".to_owned()),
            }
        }
        for (i, word) in words.iter().enumerate() {
            if [
                "load_native_certs",
                "openssl_probe",
                "env",
                "var_os",
                "set_var",
                "remove_var",
                "AWS_CA_BUNDLE",
            ]
            .contains(&word.as_str())
                || (word == "from_process" && (i == 0 || words[i - 1] != "fn"))
                || (word == "RootEnv" && words.get(i + 1).is_some_and(|v| v == "as"))
                || (word == "from_process"
                    && i >= 3
                    && words[i - 3] == "RootEnv"
                    && words[i - 2] == ":"
                    && words[i - 1] == ":")
            {
                return Err(format!("forbidden token: {word}"));
            }
        }
        Ok(())
    }
    let mut file = syn::parse_file(source).map_err(|e| e.to_string())?;
    let guard = "cfg (not (all (unix , not (target_os = \"macos\"))))";
    let guarded = |attrs: &[syn::Attribute]| {
        attrs
            .iter()
            .any(|a| a.meta.to_token_stream().to_string() == guard)
    };
    file.items.retain(|item| !matches!(item, Item::Mod(m) if m.ident == "tests" && m.attrs.iter().any(|a| a.meta.to_token_stream().to_string() == "cfg (test)")));
    for item in &mut file.items {
        if let Item::Impl(imp) = item {
            for member in &mut imp.items {
                if let ImplItem::Fn(f) = member {
                    if f.sig.ident == "native_store" && guarded(&f.attrs) {
                        f.block.stmts.clear();
                    }
                    if f.sig.ident == "from_process" {
                        f.block.stmts.retain(|stmt| !matches!(stmt, Stmt::Local(local) if guarded(&local.attrs) && matches!(&local.pat, syn::Pat::Ident(p) if p.ident == "presence") && local.init.as_ref().is_some_and(|i| i.expr.to_token_stream().to_string() == "RootEnv :: from_process ()")));
                    }
                }
            }
        }
    }
    // A from_process import can alias the call and evade path matching.
    for item in &file.items {
        if let Item::Use(u) = item {
            if u.to_token_stream()
                .to_string()
                .split_whitespace()
                .any(|s| s == "from_process")
            {
                return Err("forbidden from_process import".into());
            }
        }
    }
    scan(file.into_token_stream())
}

#[test]
fn unix_payment_source_does_not_use_environment_sensitive_loaders() {
    source_gate(include_str!("../src/sdk/roots_payment.rs")).unwrap();
    for fixture in [
        "use std::env; fn f() { env::var_os(\"SSL_CERT_FILE\"); }",
        "use openssl_probe::probe; fn f() { probe(); }",
        "use rustls_native_certs::load_native_certs; fn f() { load_native_certs(); }",
        "use rustls_native_certs::load_native_certs as load; fn f() { load(); }",
        "use std::env as e; fn f() { e::var_os(\"x\"); }",
        "use crate::RootEnv as E; fn f() { E::from_process(); }",
        "use crate::RootEnv::from_process as read; fn f() { read(); }",
        "fn f() { RootEnv::from_process(); }",
        "#[cfg(test)] fn helper() {} fn f() { openssl_probe::probe(); }",
    ] {
        assert!(source_gate(fixture).is_err(), "bypass accepted: {fixture}");
    }
}

#[cfg(target_os = "linux")]
#[test]
fn recorder_selects_first_file_and_all_directories_for_every_environment() {
    use rcgen::{BasicConstraints, CertificateParams, IsCa, KeyPair};
    struct Positive {
        calls: RefCell<Vec<String>>,
        cert: rustls::pki_types::CertificateDer<'static>,
    }
    impl PaymentRootLoader for Positive {
        fn exists(&self, p: &Path) -> bool {
            self.calls
                .borrow_mut()
                .push(format!("exists:{}", p.display()));
            matches!(
                p.to_str().unwrap(),
                "/etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem"
                    | "/etc/pki/tls/certs/ca-bundle.crt"
                    | "/etc/ssl/certs"
                    | "/etc/security/certificates"
            )
        }
        fn load_paths(&self, f: Option<&Path>, d: Option<&Path>) -> LoadedCerts {
            self.calls
                .borrow_mut()
                .push(format!("load:{}", f.or(d).unwrap().display()));
            LoadedCerts {
                certs: vec![self.cert.clone(), self.cert.clone()],
                errors: 0,
            }
        }
        fn native_store(&self) -> LoadedCerts {
            panic!("native store must not be called")
        }
    }
    let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let cert = params
        .self_signed(&KeyPair::generate().unwrap())
        .unwrap()
        .der()
        .clone();
    let expected = [
        "exists:/etc/ssl/certs/ca-certificates.crt",
        "exists:/etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem",
        "load:/etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem",
        "exists:/etc/ssl/certs",
        "load:/etc/ssl/certs",
        "exists:/etc/pki/tls/certs",
        "exists:/etc/security/certificates",
        "load:/etc/security/certificates",
    ];
    for env in [
        RootEnv::default(),
        RootEnv {
            file: Some("/hostile".into()),
            dir: Some("/hostile-dir".into()),
        },
        RootEnv {
            file: Some("".into()),
            dir: Some(":".into()),
        },
    ] {
        let loader = Positive {
            calls: RefCell::new(Vec::new()),
            cert: cert.clone(),
        };
        let roots =
            validate_payment_roots(&env, &loader, PaymentRootPlatform::UnixFixedTable).unwrap();
        assert_eq!(*loader.calls.borrow(), expected);
        assert_eq!(roots.roots().certificates(), std::slice::from_ref(&cert));
    }
}

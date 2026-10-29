//! R-ROOTS units: the pure validator over fake loaders (every row and the
//! platform tolerated-errors rule), then the real loaders over temp files.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use rustls_pki_types::pem::PemObject;

use super::*;
use crate::sdk::test_support::{Pki, TempDir};

fn cert(name: &str) -> CertificateDer<'static> {
    Pki::new(name).ca_der
}

fn garbage() -> CertificateDer<'static> {
    CertificateDer::from(b"not a certificate".to_vec())
}

fn ok(certs: &[CertificateDer<'static>]) -> LoadedCerts {
    LoadedCerts {
        certs: certs.to_vec(),
        errors: 0,
    }
}

fn err(certs: &[CertificateDer<'static>], errors: usize) -> LoadedCerts {
    LoadedCerts {
        certs: certs.to_vec(),
        errors,
    }
}

/// Loaders answering from fixed tables, recording what was asked.
#[derive(Default)]
struct FakeLoader {
    paths: BTreeMap<PathBuf, LoadedCerts>,
    native: LoadedCerts,
    probe: LoadedCerts,
    asked: std::cell::RefCell<Vec<String>>,
}

impl FakeLoader {
    fn path(mut self, path: &str, loaded: LoadedCerts) -> Self {
        self.paths.insert(PathBuf::from(path), loaded);
        self
    }

    fn lookup(&self, kind: &str, path: &Path) -> LoadedCerts {
        self.asked
            .borrow_mut()
            .push(format!("{kind}:{}", path.display()));
        self.paths.get(path).cloned().unwrap_or(LoadedCerts {
            certs: Vec::new(),
            errors: 1,
        })
    }

    fn asked(&self) -> Vec<String> {
        self.asked.borrow().clone()
    }
}

impl RootLoader for FakeLoader {
    fn load_file(&self, path: &Path) -> LoadedCerts {
        self.lookup("file", path)
    }

    fn load_dir(&self, path: &Path) -> LoadedCerts {
        self.lookup("dir", path)
    }

    fn load_native(&self) -> LoadedCerts {
        self.asked.borrow_mut().push("native".to_string());
        self.native.clone()
    }

    fn load_probe(&self) -> LoadedCerts {
        self.asked.borrow_mut().push("probe".to_string());
        self.probe.clone()
    }
}

fn env(file: Option<&str>, dir: Option<&str>) -> RootEnv {
    RootEnv {
        file: file.map(OsString::from),
        dir: dir.map(OsString::from),
    }
}

fn validate(env: &RootEnv, loader: &FakeLoader) -> Result<ValidatedRoots, RootsFailure> {
    validate_roots(env, loader, RootPlatform::UnixProbe)
}

#[test]
fn the_fixed_diagnostic_has_a_closed_reason_set_and_no_path() {
    let all = [
        (
            RootsFailure::ConfiguredLocationError,
            "configured-location-error",
        ),
        (
            RootsFailure::ConfiguredLocationEmpty,
            "configured-location-empty",
        ),
        (
            RootsFailure::ConfiguredInvalidCertificate,
            "configured-invalid-certificate",
        ),
        (RootsFailure::DirListEmpty, "dir-list-empty"),
        (RootsFailure::NoRoots, "no-roots"),
        (
            RootsFailure::EmptyFileVarUnsupported,
            "empty-file-var-unsupported",
        ),
    ];
    for (failure, reason) in all {
        assert_eq!(failure.reason(), reason);
        assert_eq!(
            failure.message(),
            format!(
                "TLS trust roots are unusable ({reason}); fix or unset SSL_CERT_FILE/SSL_CERT_DIR"
            )
        );
        let error = failure.to_error();
        assert_eq!(error.code(), "aws_invalid_config");
        assert!(error.to_string().contains(reason), "{error}");
    }
}

#[test]
fn configured_file_replaces_the_platform_store() {
    let (a, b) = (cert("file-a"), cert("file-b"));
    let loader = FakeLoader {
        native: ok(&[cert("platform")]),
        ..FakeLoader::default()
    }
    .path("/certs/bundle.pem", ok(&[a.clone(), b.clone(), a.clone()]));
    let roots = validate(&env(Some("/certs/bundle.pem"), None), &loader).expect("roots");
    // T-TLS-11: exactly the file's certificates, de-duplicated; no platform.
    let mut expected = vec![a, b];
    expected.sort_unstable_by(|a, b| a.as_ref().cmp(b.as_ref()));
    assert_eq!(roots.certificates(), expected.as_slice());
    assert_eq!(roots.mode(), RootMode::Configured);
    assert_eq!(roots.tolerated_errors(), 0);
    assert_eq!(loader.asked(), vec!["file:/certs/bundle.pem"]);
}

#[test]
fn each_configured_location_must_load_cleanly_and_contribute() {
    let good = cert("good");
    let cases: Vec<(RootEnv, FakeLoader, RootsFailure)> = vec![
        // T-TLS-5/6/7 and the mixed T-TLS-12/13/15 rows.
        (
            env(Some("/empty.pem"), None),
            FakeLoader::default().path("/empty.pem", ok(&[])),
            RootsFailure::ConfiguredLocationEmpty,
        ),
        (
            env(Some("/missing.pem"), None),
            FakeLoader::default(),
            RootsFailure::ConfiguredLocationError,
        ),
        (
            env(None, Some("/emptydir")),
            FakeLoader::default().path("/emptydir", ok(&[])),
            RootsFailure::ConfiguredLocationEmpty,
        ),
        (
            env(Some("/missing.pem"), Some("/gooddir")),
            FakeLoader::default().path("/gooddir", ok(std::slice::from_ref(&good))),
            RootsFailure::ConfiguredLocationError,
        ),
        (
            env(Some("/good.pem"), Some("/baddir")),
            FakeLoader::default()
                .path("/good.pem", ok(std::slice::from_ref(&good)))
                .path("/baddir", err(std::slice::from_ref(&good), 1)),
            RootsFailure::ConfiguredLocationError,
        ),
        (
            env(Some("/good.pem"), Some("/gooddir:/emptydir")),
            FakeLoader::default()
                .path("/good.pem", ok(std::slice::from_ref(&good)))
                .path("/gooddir", ok(std::slice::from_ref(&good)))
                .path("/emptydir", ok(&[])),
            RootsFailure::ConfiguredLocationEmpty,
        ),
        // T-TLS-14: a well-formed PEM section whose DER is not a certificate.
        (
            env(Some("/good.pem"), Some("/invaliddir")),
            FakeLoader::default()
                .path("/good.pem", ok(std::slice::from_ref(&good)))
                .path("/invaliddir", ok(&[garbage()])),
            RootsFailure::ConfiguredInvalidCertificate,
        ),
    ];
    for (env, loader, expected) in cases {
        assert_eq!(validate(&env, &loader).unwrap_err(), expected, "{env:?}");
        assert!(!loader.asked().contains(&"native".to_string()), "{env:?}");
    }
}

#[test]
fn a_directory_list_with_no_entries_is_refused() {
    for dirs in [":", "::"] {
        let loader = FakeLoader::default();
        assert_eq!(
            validate(&env(None, Some(dirs)), &loader).unwrap_err(),
            RootsFailure::DirListEmpty
        );
        assert!(loader.asked().is_empty());
    }
    // Empty entries are dropped; the rest load.
    let loader = FakeLoader::default().path("/a", ok(&[cert("a")]));
    assert!(validate(&env(None, Some(":/a:")), &loader).is_ok());
    assert_eq!(loader.asked(), vec!["dir:/a"]);
}

#[test]
fn empty_values_are_unset_and_take_the_platform_branch() {
    let platform = cert("platform");
    // T-TLS-16: `SSL_CERT_DIR=""` with no `SSL_CERT_FILE`, and both unset.
    for env in [env(None, Some("")), env(None, None)] {
        let loader = FakeLoader {
            native: ok(std::slice::from_ref(&platform)),
            ..FakeLoader::default()
        };
        let roots = validate(&env, &loader).expect("platform roots");
        assert_eq!(roots.certificates(), std::slice::from_ref(&platform));
        assert_eq!(roots.mode(), RootMode::Platform);
        assert_eq!(loader.asked(), vec!["native"]);
    }
    // T-TLS-17 on Linux: `SSL_CERT_FILE=""` loads the probe locations.
    for env in [env(Some(""), None), env(Some(""), Some(""))] {
        let loader = FakeLoader {
            probe: ok(std::slice::from_ref(&platform)),
            ..FakeLoader::default()
        };
        let roots = validate(&env, &loader).expect("probe roots");
        assert_eq!(roots.mode(), RootMode::Platform);
        assert_eq!(loader.asked(), vec!["probe"]);
    }
}

#[test]
fn an_empty_file_variable_is_refused_where_no_loader_skips_it() {
    // T-TLS-17 on macOS and Windows (PM ruling on #5016).
    let loader = FakeLoader {
        native: ok(&[cert("platform")]),
        ..FakeLoader::default()
    };
    assert_eq!(
        validate_roots(
            &env(Some(""), None),
            &loader,
            RootPlatform::NoEmptyFileLoader
        )
        .unwrap_err(),
        RootsFailure::EmptyFileVarUnsupported
    );
    assert!(loader.asked().is_empty());
    // An absent file variable still takes the native branch there.
    assert!(validate_roots(&env(None, None), &loader, RootPlatform::NoEmptyFileLoader).is_ok());
    assert_eq!(
        RootPlatform::current(),
        if cfg!(all(unix, not(target_os = "macos"))) {
            RootPlatform::UnixProbe
        } else {
            RootPlatform::NoEmptyFileLoader
        }
    );
}

#[test]
fn platform_errors_are_tolerated_only_when_a_root_loaded() {
    let good = cert("platform");
    let loader = FakeLoader {
        native: err(&[good.clone(), garbage()], 3),
        ..FakeLoader::default()
    };
    let roots = validate(&env(None, None), &loader).expect("roots");
    assert_eq!(roots.certificates(), std::slice::from_ref(&good));
    // Three load errors plus one certificate rustls rejected.
    assert_eq!(roots.tolerated_errors(), 4);
    // T-TLS-19: zero usable platform roots, with and without errors.
    for native in [ok(&[]), err(&[], 2), err(&[garbage()], 0)] {
        let loader = FakeLoader {
            native,
            ..FakeLoader::default()
        };
        assert_eq!(
            validate(&env(None, None), &loader).unwrap_err(),
            RootsFailure::NoRoots
        );
    }
    let loader = FakeLoader::default();
    assert_eq!(
        validate(&env(Some(""), None), &loader).unwrap_err(),
        RootsFailure::NoRoots
    );
}

#[test]
fn validated_roots_feed_smithy_without_reaching_its_expects() {
    // Smithy's custom-certificate path `expect`s that each PEM parses and that
    // `RootCertStore::add` accepts it; every validated root must pass both.
    let loader = FakeLoader::default().path("/b.pem", ok(&[cert("one"), cert("two")]));
    let roots = validate(&env(Some("/b.pem"), None), &loader).expect("roots");
    let pems = roots.pem_certificates();
    assert_eq!(pems.len(), 2);
    for (pem, der) in pems.iter().zip(roots.certificates()) {
        let parsed: Vec<CertificateDer<'_>> = CertificateDer::pem_slice_iter(pem)
            .collect::<Result<_, _>>()
            .expect("valid PEM certificate");
        assert_eq!(parsed, vec![der.clone()]);
        let mut store = RootCertStore::empty();
        for cert in parsed {
            store.add(cert).expect("cert parsable");
        }
    }
    assert!(roots.tls_context().is_ok());
    let clone = roots.clone();
    assert!(roots.same_allocation(&clone));
}

// --- The real loaders (rustls-native-certs) over temp files. ---

fn native(file: Option<&Path>, dir: Option<&Path>) -> Result<ValidatedRoots, RootsFailure> {
    let env = RootEnv {
        file: file.map(|p| p.as_os_str().to_owned()),
        dir: dir.map(|p| p.as_os_str().to_owned()),
    };
    validate_roots(&env, &NativeLoader, RootPlatform::current())
}

#[test]
fn real_loader_rows_for_configured_locations() {
    let tmp = TempDir::new("roots");
    let pki = Pki::new("synthetic-ca");
    let bundle = tmp.write("ca.pem", &pki.ca_pem);
    let roots = native(Some(&bundle), None).expect("file roots");
    assert_eq!(roots.certificates(), std::slice::from_ref(&pki.ca_der));

    let good_dir = tmp.dir("good");
    std::fs::write(good_dir.join("ca.pem"), &pki.ca_pem).expect("fixture");
    assert!(native(None, Some(&good_dir)).is_ok());

    let empty_file = tmp.write("empty.pem", "");
    let empty_dir = tmp.dir("emptydir");
    let missing = tmp.path().join("missing.pem");
    let missing_dir = tmp.path().join("missing-dir");
    let malformed_dir = tmp.dir("malformed");
    std::fs::write(
        malformed_dir.join("bad.pem"),
        "-----BEGIN CERTIFICATE-----\n!!!not base64!!!\n-----END CERTIFICATE-----\n",
    )
    .expect("fixture");
    let unterminated_dir = tmp.dir("unterminated");
    std::fs::write(
        unterminated_dir.join("bad.pem"),
        "-----BEGIN CERTIFICATE-----\nMIIB\n",
    )
    .expect("fixture");
    let invalid_dir = tmp.dir("invalid");
    std::fs::write(
        invalid_dir.join("notcert.pem"),
        "-----BEGIN CERTIFICATE-----\nbm90IGEgY2VydGlmaWNhdGU=\n-----END CERTIFICATE-----\n",
    )
    .expect("fixture");
    let mut rows: Vec<(Option<PathBuf>, Option<OsString>, RootsFailure)> = vec![
        (
            Some(empty_file),
            None,
            RootsFailure::ConfiguredLocationEmpty,
        ),
        (
            Some(missing.clone()),
            None,
            RootsFailure::ConfiguredLocationError,
        ),
        (
            None,
            Some(empty_dir.clone().into()),
            RootsFailure::ConfiguredLocationEmpty,
        ),
        (
            Some(missing),
            Some(good_dir.clone().into()),
            RootsFailure::ConfiguredLocationError,
        ),
        (
            Some(bundle.clone()),
            Some(missing_dir.into()),
            RootsFailure::ConfiguredLocationError,
        ),
        (
            Some(bundle.clone()),
            Some(malformed_dir.into()),
            RootsFailure::ConfiguredLocationError,
        ),
        (
            Some(bundle.clone()),
            Some(unterminated_dir.into()),
            RootsFailure::ConfiguredLocationError,
        ),
        (
            Some(bundle.clone()),
            Some(invalid_dir.into()),
            RootsFailure::ConfiguredInvalidCertificate,
        ),
        (
            None,
            Some(std::env::join_paths([&good_dir, &empty_dir]).expect("dir list")),
            RootsFailure::ConfiguredLocationEmpty,
        ),
    ];
    #[cfg(unix)]
    {
        let loop_dir = tmp.dir("loop");
        std::os::unix::fs::symlink(loop_dir.join("a"), loop_dir.join("b")).expect("symlink");
        std::os::unix::fs::symlink(loop_dir.join("b"), loop_dir.join("a")).expect("symlink");
        rows.push((
            Some(bundle.clone()),
            Some(loop_dir.into()),
            RootsFailure::ConfiguredLocationError,
        ));
    }
    for (file, dir, expected) in rows {
        let env = RootEnv {
            file: file.as_ref().map(|p| p.as_os_str().to_owned()),
            dir: dir.clone(),
        };
        assert_eq!(
            validate_roots(&env, &NativeLoader, RootPlatform::current()).unwrap_err(),
            expected,
            "{file:?} {dir:?}"
        );
    }
}

//! Payment-only platform trust roots, isolated from caller certificate overrides.
use std::path::Path;

use super::roots::{parsable, LoadedCerts, RootEnv, RootMode, ValidatedRoots};

/// Roots for proof-bearing payment connections. Only the payment validator can construct these.
///
/// ```compile_fail
/// use toolkit_aws::sdk::{roots::ValidatedRoots, roots_payment::PaymentRoots};
/// fn bad(roots: ValidatedRoots) -> PaymentRoots { PaymentRoots(roots) }
/// ```
/// ```compile_fail
/// use toolkit_aws::sdk::{roots::ValidatedRoots, roots_payment::PaymentRoots};
/// fn bad(roots: ValidatedRoots) -> PaymentRoots { roots.into() }
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaymentRoots(ValidatedRoots);

/// Platform loading strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaymentRootPlatform {
    /// Load only the fixed Unix locations.
    UnixFixedTable,
    /// Refuse overrides before loading the native store.
    NativeStoreOnly,
}
impl PaymentRootPlatform {
    /// Strategy for the current target.
    #[must_use]
    pub fn current() -> Self {
        if cfg!(all(unix, not(target_os = "macos"))) {
            Self::UnixFixedTable
        } else {
            Self::NativeStoreOnly
        }
    }
}

/// Fixed, path-free failures for payment roots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaymentRootsFailure {
    /// No parsable roots were found.
    NoRoots,
    /// An override is present where the native loader would read it.
    OverrideVariablePresent,
}
impl PaymentRootsFailure {
    /// The fixed, path-free diagnostic.
    #[must_use]
    pub fn message(self) -> &'static str {
        match self {
            Self::NoRoots => "Payment TLS trust roots are unusable (no-roots); install the platform CA certificates.",
            Self::OverrideVariablePresent => "Payment TLS trust roots ignore SSL_CERT_FILE/SSL_CERT_DIR; unset them to make payments on this platform.",
        }
    }
}

/// Explicit path and native-store loader, injectable for offline validation.
pub trait PaymentRootLoader {
    /// Load exactly the supplied file or directory.
    fn load_paths(&self, file: Option<&Path>, dir: Option<&Path>) -> LoadedCerts;
    /// Load the native store, used only after presence validation.
    fn native_store(&self) -> LoadedCerts;
    /// Whether a fixed candidate location exists.
    fn exists(&self, path: &Path) -> bool;
}

// Restated from openssl-probe 0.2.2 CERTIFICATE_FILE_NAMES and CERTIFICATE_DIRS.
fn locations() -> (&'static [&'static str], &'static [&'static str]) {
    let files: &[&str] = if cfg!(target_os = "linux") {
        &[
            "/etc/ssl/certs/ca-certificates.crt",
            "/etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem",
            "/etc/pki/tls/certs/ca-bundle.crt",
            "/etc/ssl/ca-bundle.pem",
            "/etc/pki/tls/cacert.pem",
            "/etc/ssl/cert.pem",
            "/opt/etc/ssl/certs/ca-certificates.crt",
            "/etc/ssl/certs/cacert.pem",
        ]
    } else if cfg!(target_os = "freebsd") {
        &["/usr/local/etc/ssl/cert.pem"]
    } else if cfg!(target_os = "dragonfly") {
        &["/usr/local/share/certs/ca-root-nss.crt"]
    } else if cfg!(target_os = "netbsd") {
        &["/etc/openssl/certs/ca-certificates.crt"]
    } else if cfg!(target_os = "openbsd") {
        &["/etc/ssl/cert.pem"]
    } else if cfg!(target_os = "solaris") {
        &["/etc/certs/ca-certificates.crt"]
    } else if cfg!(target_os = "illumos") {
        &[
            "/etc/ssl/cacert.pem",
            "/etc/certs/ca-certificates.crt",
            "/etc/ssl/certs/ca-certificates.crt",
        ]
    } else if cfg!(target_os = "android") {
        &["/data/data/com.termux/files/usr/etc/tls/cert.pem"]
    } else if cfg!(target_os = "haiku") {
        &["/boot/system/data/ssl/CARootCertificates.pem"]
    } else {
        &["/etc/ssl/certs/ca-certificates.crt"]
    };
    let dirs: &[&str] = if cfg!(target_os = "linux") {
        &[
            "/etc/ssl/certs",
            "/etc/pki/tls/certs",
            "/etc/security/certificates",
        ]
    } else if cfg!(target_os = "freebsd") {
        &["/etc/ssl/certs", "/usr/local/share/certs"]
    } else if cfg!(any(target_os = "illumos", target_os = "solaris")) {
        &["/etc/certs/CA"]
    } else if cfg!(target_os = "netbsd") {
        &["/etc/openssl/certs"]
    } else if cfg!(target_os = "aix") {
        &["/var/ssl/certs"]
    } else {
        &["/etc/ssl/certs"]
    };
    (files, dirs)
}

/// Validate platform payment roots. Unix ignores the supplied environment view.
///
/// # Errors
/// Returns a fixed failure if overrides are present on a native-store target or no usable roots exist.
pub fn validate_payment_roots(
    presence: &RootEnv,
    loader: &dyn PaymentRootLoader,
    platform: PaymentRootPlatform,
) -> Result<PaymentRoots, PaymentRootsFailure> {
    let mut loaded = LoadedCerts::default();
    match platform {
        PaymentRootPlatform::NativeStoreOnly => {
            if presence.file.is_some() || presence.dir.is_some() {
                return Err(PaymentRootsFailure::OverrideVariablePresent);
            }
            loaded = loader.native_store();
        }
        PaymentRootPlatform::UnixFixedTable => {
            let (files, dirs) = locations();
            let file = files.iter().map(Path::new).find(|p| loader.exists(p));
            let entries = file
                .map(|p| loader.load_paths(Some(p), None))
                .into_iter()
                .chain(
                    dirs.iter()
                        .map(Path::new)
                        .filter(|p| loader.exists(p))
                        .map(|p| loader.load_paths(None, Some(p))),
                );
            for entry in entries {
                loaded.certs.extend(entry.certs);
                loaded.errors += entry.errors;
            }
        }
    }
    let (certs, rejected) = parsable(loaded.certs);
    ValidatedRoots::new(certs, RootMode::PaymentPlatform, loaded.errors + rejected)
        .map(PaymentRoots)
        .ok_or(PaymentRootsFailure::NoRoots)
}

impl PaymentRoots {
    /// Read the platform store without accepting environment-provided certificates.
    ///
    /// # Errors
    /// Returns a fixed payment roots failure if the platform store is unusable.
    pub fn from_process() -> Result<Self, PaymentRootsFailure> {
        #[cfg(all(unix, not(target_os = "macos")))]
        let presence = RootEnv::default();
        #[cfg(not(all(unix, not(target_os = "macos"))))]
        let presence = RootEnv::from_process();
        validate_payment_roots(&presence, &PlatformLoader, PaymentRootPlatform::current())
    }
    /// Borrow the validated roots for an explicit TLS context.
    #[must_use]
    pub fn roots(&self) -> &ValidatedRoots {
        &self.0
    }
}

struct PlatformLoader;
impl PaymentRootLoader for PlatformLoader {
    fn load_paths(&self, file: Option<&Path>, dir: Option<&Path>) -> LoadedCerts {
        let result = rustls_native_certs::load_certs_from_paths(file, dir);
        LoadedCerts {
            certs: result.certs,
            errors: result.errors.len(),
        }
    }
    #[cfg(not(all(unix, not(target_os = "macos"))))]
    fn native_store(&self) -> LoadedCerts {
        let result = rustls_native_certs::load_native_certs();
        LoadedCerts {
            certs: result.certs,
            errors: result.errors.len(),
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    fn native_store(&self) -> LoadedCerts {
        LoadedCerts::default()
    }
    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sdk::test_support::{child_case, child_command, report, run_child, Pki, TempDir};
    use rustls::{ClientConfig, ClientConnection, RootCertStore, ServerConfig, ServerConnection};
    use std::sync::Arc;

    fn handshake(roots: &ValidatedRoots, pki: &Pki) -> bool {
        let (chain, key) = pki.server("localhost");
        handshake_chain(roots, chain, key)
    }

    fn handshake_chain(
        roots: &ValidatedRoots,
        chain: Vec<rustls::pki_types::CertificateDer<'static>>,
        key: rustls::pki_types::PrivateKeyDer<'static>,
    ) -> bool {
        let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
        let mut store = RootCertStore::empty();
        for cert in roots.certificates() {
            store.add(cert.clone()).unwrap();
        }
        let client_config = ClientConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(store)
            .with_no_client_auth();
        let server_config = ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(chain, key)
            .unwrap();
        let mut client =
            ClientConnection::new(Arc::new(client_config), "localhost".try_into().unwrap())
                .unwrap();
        let mut server = ServerConnection::new(Arc::new(server_config)).unwrap();
        for _ in 0..20 {
            let mut data = Vec::new();
            client.write_tls(&mut data).unwrap();
            server.read_tls(&mut data.as_slice()).unwrap();
            if server.process_new_packets().is_err() {
                return false;
            }
            let mut data = Vec::new();
            server.write_tls(&mut data).unwrap();
            client.read_tls(&mut data.as_slice()).unwrap();
            if client.process_new_packets().is_err() {
                return false;
            }
            if !client.is_handshaking() && !server.is_handshaking() {
                return true;
            }
        }
        false
    }

    struct Fake(LoadedCerts);
    impl PaymentRootLoader for Fake {
        fn exists(&self, _: &Path) -> bool {
            true
        }
        fn load_paths(&self, _: Option<&Path>, _: Option<&Path>) -> LoadedCerts {
            self.0.clone()
        }
        fn native_store(&self) -> LoadedCerts {
            self.0.clone()
        }
    }

    #[test]
    fn platform_roots_validate_deduplicate_and_handshake() {
        let pki = Pki::new("payment platform");
        let fake = Fake(LoadedCerts {
            certs: vec![
                pki.ca_der.clone(),
                pki.ca_der.clone(),
                b"bad".to_vec().into(),
            ],
            errors: 2,
        });
        let roots = validate_payment_roots(
            &RootEnv::default(),
            &fake,
            PaymentRootPlatform::NativeStoreOnly,
        )
        .unwrap();
        assert_eq!(roots.roots().mode(), RootMode::PaymentPlatform);
        assert_eq!(roots.roots().certificates().len(), 1);
        assert_eq!(roots.roots().tolerated_errors(), 3);
        assert!(handshake(roots.roots(), &pki));
        assert!(!handshake(roots.roots(), &Pki::new("untrusted")));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn child_real_roots() {
        if child_case().is_none() {
            return;
        }
        let roots = PaymentRoots::from_process().unwrap();
        let digest = roots
            .roots()
            .certificates()
            .iter()
            .map(|c| aws_smithy_types::base64::encode(c.as_ref()))
            .collect::<Vec<_>>()
            .join(",");
        report("roots", digest);
        if let Some(file) = std::env::var_os("PAYMENT_TEST_CA") {
            let pki_der = std::fs::read(file).unwrap();
            assert!(!roots
                .roots()
                .certificates()
                .iter()
                .any(|c| c.as_ref() == pki_der));
            if std::env::var_os("SSL_CERT_FILE")
                .is_some_and(|v| !v.is_empty() && v != ":" && Path::new(&v).exists())
            {
                let provider = ValidatedRoots::from_process().unwrap();
                assert!(provider
                    .certificates()
                    .iter()
                    .any(|c| c.as_ref() == pki_der));
                let leaf = std::fs::read(std::env::var_os("PAYMENT_TEST_LEAF").unwrap()).unwrap();
                let key = std::fs::read(std::env::var_os("PAYMENT_TEST_KEY").unwrap()).unwrap();
                let chain = vec![leaf.into(), pki_der.into()];
                let key = rustls::pki_types::PrivateKeyDer::Pkcs8(key.into());
                assert!(!handshake_chain(
                    roots.roots(),
                    chain.clone(),
                    key.clone_key()
                ));
                assert!(handshake_chain(&provider, chain, key));
            }
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn real_loader_ignores_hostile_and_empty_overrides() {
        let temp = TempDir::new("payment-roots");
        let pki = Pki::new("hostile");
        let pem = temp.write("hostile.pem", &pki.ca_pem);
        let der = temp.write("hostile.der", pki.ca_der.as_ref());
        let (chain, key) = pki.server("localhost");
        let leaf = temp.write("leaf.der", chain[0].as_ref());
        let key = temp.write("leaf-key.der", key.secret_der());
        let dir = temp.dir("certs");
        std::fs::write(dir.join("hostile.pem"), &pki.ca_pem).unwrap();
        let entry = "sdk::roots_payment::tests::child_real_roots";
        let baseline = run_child(&mut child_command(entry, "baseline"));
        baseline.assert_ok();
        for (file, dirs, aws) in [
            (Some(pem.as_os_str()), None, None),
            (None, Some(dir.as_os_str()), None),
            (Some(pem.as_os_str()), Some(dir.as_os_str()), None),
            (None, None, Some(pem.as_os_str())),
            (
                Some(std::ffi::OsStr::new("")),
                Some(std::ffi::OsStr::new("")),
                None,
            ),
            (
                Some(std::ffi::OsStr::new(":")),
                Some(std::ffi::OsStr::new(":")),
                None,
            ),
            (
                Some(std::ffi::OsStr::new("/nonexistent-payment-root")),
                Some(std::ffi::OsStr::new("/nonexistent-payment-root")),
                None,
            ),
        ] {
            let mut cmd = child_command(entry, "override");
            cmd.env("PAYMENT_TEST_CA", &der);
            cmd.env("PAYMENT_TEST_LEAF", &leaf)
                .env("PAYMENT_TEST_KEY", &key);
            if let Some(v) = file {
                cmd.env("SSL_CERT_FILE", v);
            }
            if let Some(v) = dirs {
                cmd.env("SSL_CERT_DIR", v);
            }
            if let Some(v) = aws {
                cmd.env("AWS_CA_BUNDLE", v);
            }
            let child = run_child(&mut cmd);
            child.assert_ok();
            assert_eq!(child.result("roots"), baseline.result("roots"));
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_table_is_pinned() {
        let (files, dirs) = locations();
        assert_eq!(
            files,
            [
                "/etc/ssl/certs/ca-certificates.crt",
                "/etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem",
                "/etc/pki/tls/certs/ca-bundle.crt",
                "/etc/ssl/ca-bundle.pem",
                "/etc/pki/tls/cacert.pem",
                "/etc/ssl/cert.pem",
                "/opt/etc/ssl/certs/ca-certificates.crt",
                "/etc/ssl/certs/cacert.pem"
            ]
        );
        assert_eq!(
            dirs,
            [
                "/etc/ssl/certs",
                "/etc/pki/tls/certs",
                "/etc/security/certificates"
            ]
        );
    }
}

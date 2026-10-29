//! R-ROOTS: explicit TLS trust-root validation (#5014, design 4.3).
//!
//! Smithy's native-root path keeps whatever loaded (a broken `SSL_CERT_FILE`
//! next to a valid `SSL_CERT_DIR` still verifies) and hits a `debug_assert!`
//! with zero roots. This module never uses it. Roots are validated **once**,
//! before any connector or credential provider is built, by [`validate_roots`]:
//! a pure function over an environment view ([`RootEnv`]) and injected
//! loaders ([`RootLoader`]). Every failure is terminal and has a fixed,
//! path-free reason ([`RootsFailure`]). The result, [`ValidatedRoots`], cannot
//! be empty, and [`ValidatedRoots::tls_context`] gives both planes (the
//! `SdkTransport` connector and the provider-plane HTTP client) an explicit
//! trust store with native roots **disabled**.
//!
//! 1. An unset or empty variable is unset. A non-empty `SSL_CERT_DIR` is split
//!    with [`std::env::split_paths`] and empty entries are dropped; none left
//!    is [`RootsFailure::DirListEmpty`].
//! 2. Configured mode (either variable non-empty): the file and each directory
//!    load separately; any load error, any location without a certificate, or
//!    any certificate rustls rejects fails closed. The platform store is never
//!    consulted.
//! 3. Platform mode: `load_native_certs()` when `SSL_CERT_FILE` is absent; when
//!    it is present but empty, the Unix probe locations on Linux and other
//!    non-macOS Unix, and a refusal on macOS and Windows (PM ruling on #5016,
//!    proposed ruling R-7). Loading errors are tolerated only when at least
//!    one root loaded; their **count** is reported, never a path or text.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use aws_smithy_http_client::tls::{TlsContext, TrustStore};
use rustls::RootCertStore;
use rustls_pki_types::CertificateDer;

use crate::error::AwsError;

/// Variable naming a certificate bundle file.
pub const SSL_CERT_FILE: &str = "SSL_CERT_FILE";

/// Variable naming certificate directories.
pub const SSL_CERT_DIR: &str = "SSL_CERT_DIR";

/// Why the trust roots are unusable. The closed reason set of the fixed
/// startup diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootsFailure {
    /// A configured location reported an error (missing, unreadable, a bad
    /// entry, a malformed PEM certificate section).
    ConfiguredLocationError,
    /// A configured location held no certificate.
    ConfiguredLocationEmpty,
    /// rustls rejected a certificate from a configured location.
    ConfiguredInvalidCertificate,
    /// `SSL_CERT_DIR` is non-empty but names no directory (for example `:`).
    DirListEmpty,
    /// The platform store yielded no usable root.
    NoRoots,
    /// `SSL_CERT_FILE` is set to the empty string on a platform whose loader
    /// would read it as a path (macOS, Windows).
    EmptyFileVarUnsupported,
}

impl RootsFailure {
    /// The fixed reason token.
    #[must_use]
    pub fn reason(self) -> &'static str {
        match self {
            Self::ConfiguredLocationError => "configured-location-error",
            Self::ConfiguredLocationEmpty => "configured-location-empty",
            Self::ConfiguredInvalidCertificate => "configured-invalid-certificate",
            Self::DirListEmpty => "dir-list-empty",
            Self::NoRoots => "no-roots",
            Self::EmptyFileVarUnsupported => "empty-file-var-unsupported",
        }
    }

    /// The fixed message (no path, no I/O or PEM error text). A command prints
    /// it after its own `toolkit <command>: ` prefix.
    #[must_use]
    pub fn message(self) -> String {
        format!(
            "TLS trust roots are unusable ({}); fix or unset SSL_CERT_FILE/SSL_CERT_DIR",
            self.reason()
        )
    }

    /// The same as a boundary error (field `tls_roots`), for commands that
    /// report through [`AwsError`].
    #[must_use]
    pub fn to_error(self) -> AwsError {
        AwsError::invalid_config("tls_roots", self.message())
    }
}

/// The two variables as the process holds them, read with
/// [`std::env::var_os`] and never changed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RootEnv {
    /// `SSL_CERT_FILE`, if present (possibly empty).
    pub file: Option<OsString>,
    /// `SSL_CERT_DIR`, if present (possibly empty).
    pub dir: Option<OsString>,
}

impl RootEnv {
    /// The current process's values.
    #[must_use]
    pub fn from_process() -> Self {
        Self {
            file: std::env::var_os(SSL_CERT_FILE),
            dir: std::env::var_os(SSL_CERT_DIR),
        }
    }
}

/// What one location produced: certificates (DER) and how many errors.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LoadedCerts {
    /// Certificates found, unvalidated.
    pub certs: Vec<CertificateDer<'static>>,
    /// Errors reported (a count only; their text is never kept).
    pub errors: usize,
}

/// Certificate loaders. Production is [`NativeLoader`]; tests inject fakes.
pub trait RootLoader {
    /// One configured file.
    fn load_file(&self, path: &Path) -> LoadedCerts;
    /// One configured directory.
    fn load_dir(&self, path: &Path) -> LoadedCerts;
    /// The platform store (`rustls_native_certs::load_native_certs`).
    fn load_native(&self) -> LoadedCerts;
    /// The Unix platform loader's locations (`openssl_probe::probe()`), each
    /// loaded separately. Only called on [`RootPlatform::UnixProbe`].
    fn load_probe(&self) -> LoadedCerts;
}

/// How an empty `SSL_CERT_FILE` is handled on this target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootPlatform {
    /// Linux and other non-macOS Unix: load the probe locations directly.
    UnixProbe,
    /// macOS and Windows: no public loader skips the variable; refuse.
    NoEmptyFileLoader,
}

impl RootPlatform {
    /// The platform this binary was built for.
    #[must_use]
    pub fn current() -> Self {
        if cfg!(all(unix, not(target_os = "macos"))) {
            Self::UnixProbe
        } else {
            Self::NoEmptyFileLoader
        }
    }
}

/// Which branch produced the roots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootMode {
    /// From `SSL_CERT_FILE`/`SSL_CERT_DIR`, replacing the platform store.
    Configured,
    /// From the platform store.
    Platform,
}

/// A non-empty, validated set of trust roots (de-duplicated by DER, sorted).
/// Cheap to clone; clones share the certificates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedRoots {
    certs: Arc<Vec<CertificateDer<'static>>>,
    mode: RootMode,
    tolerated_errors: usize,
}

impl ValidatedRoots {
    /// The only constructor: `None` for an empty set.
    fn new(
        mut certs: Vec<CertificateDer<'static>>,
        mode: RootMode,
        tolerated_errors: usize,
    ) -> Option<Self> {
        certs.sort_unstable_by(|a, b| a.as_ref().cmp(b.as_ref()));
        certs.dedup();
        if certs.is_empty() {
            return None;
        }
        Some(Self {
            certs: Arc::new(certs),
            mode,
            tolerated_errors,
        })
    }

    /// Roots from known-good certificates (test builds only).
    #[cfg(test)]
    pub(crate) fn for_tests(certs: Vec<CertificateDer<'static>>) -> Self {
        Self::new(certs, RootMode::Configured, 0).expect("at least one test root")
    }

    /// Validate the current process's roots with the real loaders.
    ///
    /// # Errors
    /// The [`RootsFailure`] of [`validate_roots`].
    pub fn from_process() -> Result<Self, RootsFailure> {
        validate_roots(
            &RootEnv::from_process(),
            &NativeLoader,
            RootPlatform::current(),
        )
    }

    /// The roots, sorted by DER.
    #[must_use]
    pub fn certificates(&self) -> &[CertificateDer<'static>] {
        &self.certs
    }

    /// Which branch produced them.
    #[must_use]
    pub fn mode(&self) -> RootMode {
        self.mode
    }

    /// Platform-mode loading errors tolerated because roots did load (a count
    /// for the diagnostics sink; always 0 in configured mode).
    #[must_use]
    pub fn tolerated_errors(&self) -> usize {
        self.tolerated_errors
    }

    /// Report tolerated platform loading errors to `diag` (a count only).
    pub fn report_to(&self, diag: &mut crate::diag::Diag) {
        if self.tolerated_errors > 0 {
            diag.emit(crate::diag::DiagEvent::TlsRootErrorsTolerated {
                count: self.tolerated_errors,
            });
        }
    }

    /// Whether two values share the same certificate set allocation (the
    /// "both planes received this value" check).
    #[must_use]
    pub fn same_allocation(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.certs, &other.certs)
    }

    /// Each root as one PEM `CERTIFICATE` block.
    #[must_use]
    pub fn pem_certificates(&self) -> Vec<Vec<u8>> {
        self.certs.iter().map(|der| pem_encode(der)).collect()
    }

    /// The explicit TLS context for a Smithy connector or client: an empty
    /// trust store (native roots disabled) plus exactly these roots.
    ///
    /// # Errors
    /// `InvalidConfig` (field `tls_roots`) if Smithy refuses the context.
    pub fn tls_context(&self) -> Result<TlsContext, AwsError> {
        let store = self
            .pem_certificates()
            .into_iter()
            .fold(TrustStore::empty(), TrustStore::with_pem_certificate);
        TlsContext::builder()
            .with_trust_store(store)
            .build()
            .map_err(|_| {
                AwsError::invalid_config("tls_roots", "the TLS context could not be built")
            })
    }
}

/// One DER certificate as PEM, 64 base64 characters per line.
fn pem_encode(der: &CertificateDer<'_>) -> Vec<u8> {
    let encoded = aws_smithy_types::base64::encode(der.as_ref());
    let mut pem = String::from("-----BEGIN CERTIFICATE-----\n");
    for line in encoded.as_bytes().chunks(64) {
        pem.push_str(&String::from_utf8_lossy(line));
        pem.push('\n');
    }
    pem.push_str("-----END CERTIFICATE-----\n");
    pem.into_bytes()
}

/// Split certificates into those rustls accepts and a count of the others.
fn parsable(certs: Vec<CertificateDer<'static>>) -> (Vec<CertificateDer<'static>>, usize) {
    let mut accepted = Vec::with_capacity(certs.len());
    let mut rejected = 0;
    for cert in certs {
        let mut store = RootCertStore::empty();
        let (valid, _) = store.add_parsable_certificates([cert.clone()]);
        if valid == 1 {
            accepted.push(cert);
        } else {
            rejected += 1;
        }
    }
    (accepted, rejected)
}

/// Validate the trust roots (R-ROOTS).
///
/// # Errors
/// The [`RootsFailure`] for the first rule broken: in configured mode a load
/// error beats an empty location, which beats an invalid certificate.
pub fn validate_roots(
    env: &RootEnv,
    loader: &dyn RootLoader,
    platform: RootPlatform,
) -> Result<ValidatedRoots, RootsFailure> {
    let file = env
        .file
        .as_ref()
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let dirs: Option<Vec<PathBuf>> =
        env.dir
            .as_ref()
            .filter(|value| !value.is_empty())
            .map(|value| {
                std::env::split_paths(value)
                    .filter(|path| !path.as_os_str().is_empty())
                    .collect()
            });
    if dirs.as_ref().is_some_and(Vec::is_empty) {
        return Err(RootsFailure::DirListEmpty);
    }
    if file.is_some() || dirs.is_some() {
        let mut locations = Vec::new();
        if let Some(path) = &file {
            locations.push(loader.load_file(path));
        }
        for dir in dirs.iter().flatten() {
            locations.push(loader.load_dir(dir));
        }
        return configured(locations);
    }
    let platform_certs = match (&env.file, platform) {
        (None, _) => loader.load_native(),
        (Some(_), RootPlatform::UnixProbe) => loader.load_probe(),
        (Some(_), RootPlatform::NoEmptyFileLoader) => {
            return Err(RootsFailure::EmptyFileVarUnsupported)
        }
    };
    let (accepted, rejected) = parsable(platform_certs.certs);
    ValidatedRoots::new(
        accepted,
        RootMode::Platform,
        platform_certs.errors + rejected,
    )
    .ok_or(RootsFailure::NoRoots)
}

fn configured(locations: Vec<LoadedCerts>) -> Result<ValidatedRoots, RootsFailure> {
    if locations.iter().any(|location| location.errors > 0) {
        return Err(RootsFailure::ConfiguredLocationError);
    }
    if locations.iter().any(|location| location.certs.is_empty()) {
        return Err(RootsFailure::ConfiguredLocationEmpty);
    }
    let all: Vec<_> = locations.into_iter().flat_map(|l| l.certs).collect();
    let (accepted, rejected) = parsable(all);
    if rejected > 0 {
        return Err(RootsFailure::ConfiguredInvalidCertificate);
    }
    ValidatedRoots::new(accepted, RootMode::Configured, 0)
        .ok_or(RootsFailure::ConfiguredLocationEmpty)
}

/// The production loaders: rustls-native-certs, and `openssl-probe` for the
/// Unix probe locations.
#[derive(Debug, Clone, Copy, Default)]
pub struct NativeLoader;

impl NativeLoader {
    fn adopt(result: rustls_native_certs::CertificateResult) -> LoadedCerts {
        LoadedCerts {
            certs: result.certs,
            errors: result.errors.len(),
        }
    }
}

impl RootLoader for NativeLoader {
    fn load_file(&self, path: &Path) -> LoadedCerts {
        Self::adopt(rustls_native_certs::load_certs_from_paths(Some(path), None))
    }

    fn load_dir(&self, path: &Path) -> LoadedCerts {
        Self::adopt(rustls_native_certs::load_certs_from_paths(None, Some(path)))
    }

    fn load_native(&self) -> LoadedCerts {
        Self::adopt(rustls_native_certs::load_native_certs())
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    fn load_probe(&self) -> LoadedCerts {
        let probe = openssl_probe::probe();
        let mut out = LoadedCerts::default();
        let files = probe.cert_file.iter().map(|file| self.load_file(file));
        let dirs = probe.cert_dir.iter().map(|dir| self.load_dir(dir));
        for loaded in files.chain(dirs) {
            out.certs.extend(loaded.certs);
            out.errors += loaded.errors;
        }
        out
    }

    #[cfg(not(all(unix, not(target_os = "macos"))))]
    fn load_probe(&self) -> LoadedCerts {
        // Never reached: `RootPlatform::current()` refuses first here.
        LoadedCerts {
            certs: Vec::new(),
            errors: 1,
        }
    }
}

#[cfg(test)]
mod tests;

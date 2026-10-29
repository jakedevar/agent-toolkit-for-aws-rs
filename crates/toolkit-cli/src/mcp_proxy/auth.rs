//! The credential seam. The protocol core signs every attempt through
//! [`Auth`] and never sees a credential. S3 implements it over S1's guarded
//! SDK sources (R-ABS, R-SRC, R-PREC, R-STICKY, D1); S2 tests use
//! [`SignerAuth`] over the fake signer, and [`Unsigned`].

use std::sync::{Arc, Mutex, PoisonError};

use toolkit_aws::diag::{Diag, DiagEvent};
use toolkit_aws::{AwsError, CredentialRefusal, Credentials, HttpRequest, Signer, SigningScope};

/// Signs MCP-plane requests.
pub trait Auth: Send + Sync {
    /// Sign one attempt (a fresh signature per attempt). An error is a
    /// credential failure: the request is not sent.
    ///
    /// # Errors
    /// Whatever the credential source reports.
    fn sign(&self, request: &mut HttpRequest) -> Result<(), AwsError>;

    /// D1: called once after a 401/403. `true` means the credentials were
    /// rebuilt and the request should be re-signed and sent once more.
    fn refresh(&self) -> bool {
        false
    }

    /// A new MCP session was established (a new `Mcp-Session-Id`).
    fn session_started(&self) {}

    /// An `initialize` request is about to be signed: the session it opens
    /// starts here (R-STICKY forgets the previous session's signing). It may
    /// wait on credential I/O, so the core calls it from the initialize
    /// worker with no proxy lock held (#5136).
    fn initialize_started(&self) {}
}

/// An [`Auth`] over any [`Signer`] and a fixed scope. The signer is shared so
/// a test can inspect it; signing is serialised by the mutex.
pub struct SignerAuth<S> {
    signer: Arc<Mutex<S>>,
    scope: SigningScope,
}

impl<S> SignerAuth<S> {
    /// Sign with `signer` for `scope`.
    #[must_use]
    pub fn new(signer: Arc<Mutex<S>>, scope: SigningScope) -> Self {
        Self { signer, scope }
    }
}

impl<S> std::fmt::Debug for SignerAuth<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SignerAuth")
            .field("scope", &self.scope)
            .finish_non_exhaustive()
    }
}

impl<S: Signer + Send> Auth for SignerAuth<S> {
    fn sign(&self, request: &mut HttpRequest) -> Result<(), AwsError> {
        self.signer
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .sign(request, &self.scope)
    }
}

/// Sends every request unsigned.
#[derive(Debug, Clone, Copy, Default)]
pub struct Unsigned;

impl Auth for Unsigned {
    fn sign(&self, _request: &mut HttpRequest) -> Result<(), AwsError> {
        Ok(())
    }
}

/// What one request does about credentials (S1's `RequestCredentials`
/// without the SDK types, so the default build can test the wiring).
#[derive(Debug)]
pub enum Resolved {
    /// Sign with these.
    Sign(Credentials),
    /// Send unsigned (`--skip-auth` and genuine absence only).
    Unsigned,
    /// Fail closed with the fixed credentials message.
    Refused(CredentialRefusal),
}

/// Guarded credential resolution (R-ABS, R-SRC, R-PREC, D1, R-STICKY): in
/// production S1's `GuardedCredentials`; in tests a counting fake (T-CRED-1).
pub trait CredentialSource: Send + Sync {
    /// Resolve one request; `force_rebuild` is D1 after a 401/403.
    fn resolve(&self, skip_auth: bool, force_rebuild: bool) -> Resolved;
    /// The MCP session ended: a new one may start unsigned (R-STICKY).
    fn end_session(&self);
}

/// Signs a request with resolved credentials (`SigV4` in production).
pub type SignWith = Box<
    dyn Fn(&Credentials, &mut HttpRequest, &SigningScope) -> Result<(), AwsError> + Send + Sync,
>;

/// The proxy's [`Auth`] over a [`CredentialSource`]:
///
/// - `sign`: one resolution per attempt; `Sign` signs, `Unsigned` sends as is,
///   `Refused` reports the typed refusal to the diagnostics sink and fails the
///   request with the fixed credentials text (nothing is sent);
/// - `refresh` (D1, after a 401/403): one forced rebuild, then the core
///   re-signs once (the rebuilt provider is reused by that attempt);
/// - `initialize_started`: R-STICKY's session boundary.
pub struct SourceAuth {
    source: Box<dyn CredentialSource>,
    sign_with: SignWith,
    scope: SigningScope,
    skip_auth: bool,
    diag: Mutex<Diag>,
}

impl SourceAuth {
    /// Sign for `scope` with `sign_with`, resolving through `source`.
    #[must_use]
    pub fn new(
        source: Box<dyn CredentialSource>,
        sign_with: SignWith,
        scope: SigningScope,
        skip_auth: bool,
        diag: Diag,
    ) -> Self {
        Self {
            source,
            sign_with,
            scope,
            skip_auth,
            diag: Mutex::new(diag),
        }
    }
}

impl std::fmt::Debug for SourceAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SourceAuth([REDACTED])")
    }
}

impl Auth for SourceAuth {
    fn sign(&self, request: &mut HttpRequest) -> Result<(), AwsError> {
        match self.source.resolve(self.skip_auth, false) {
            Resolved::Sign(credentials) => (self.sign_with)(&credentials, request, &self.scope),
            Resolved::Unsigned => Ok(()),
            Resolved::Refused(refusal) => {
                self.diag
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .emit(DiagEvent::CredentialFailure { refusal });
                Err(AwsError::NoCredentials)
            }
        }
    }

    fn refresh(&self) -> bool {
        !matches!(
            self.source.resolve(self.skip_auth, true),
            Resolved::Refused(_)
        )
    }

    fn initialize_started(&self) {
        self.source.end_session();
    }
}

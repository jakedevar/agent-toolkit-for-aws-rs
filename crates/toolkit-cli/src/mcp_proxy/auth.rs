//! The credential seam. The protocol core signs every attempt through
//! [`Auth`] and never sees a credential. S3 implements it over S1's guarded
//! SDK sources (R-ABS, R-SRC, R-PREC, R-STICKY, D1); S2 tests use
//! [`SignerAuth`] over the fake signer, and [`Unsigned`].

use std::sync::{Arc, Mutex, PoisonError};

use toolkit_aws::{AwsError, HttpRequest, Signer, SigningScope};

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

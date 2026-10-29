//! The in-process browser-handle vault (`prepare_browser_payment` and
//! `attach_browser_payment`, design section 8.1).
//!
//! A verified proof header stays in this process, referenced by a handle
//! `x402h_` plus 32 base64url characters from 24 OS-random bytes. A handle
//! carries nothing about the proof, is single use, expires after 90 s on the
//! injected monotonic clock and redeems only for the origin and path it was
//! paid for. The vault dies with the process (boundary B1, #5103).

use std::cell::RefCell;
use std::time::{Duration, Instant};

use super::net::ProofHeader;
use super::urlparse::urlparse;

/// How long a handle can be redeemed.
pub const HANDLE_TTL: Duration = Duration::from_secs(90);

/// Why a redemption was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VaultError {
    /// Unknown, already used or expired.
    Unknown,
    /// Another origin.
    Origin,
    /// Another path.
    Path,
    /// The OS random source failed.
    Random,
}

impl VaultError {
    /// The legacy `PaymentBlocked` text.
    #[must_use]
    pub fn reason(self) -> &'static str {
        match self {
            Self::Unknown => "Unknown, already-used, or expired payment handle.",
            Self::Origin => "Handle does not match this origin.",
            Self::Path => "Handle does not match this resource path.",
            Self::Random => "RuntimeError during payment flow.",
        }
    }
}

struct Entry {
    handle: String,
    header: ProofHeader,
    origin: String,
    path: String,
    expires_at: Instant,
}

/// The handle vault of one process.
#[derive(Default)]
pub struct Vault {
    entries: RefCell<Vec<Entry>>,
}

impl std::fmt::Debug for Vault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vault")
            .field("entries", &self.entries.borrow().len())
            .finish()
    }
}

impl Vault {
    /// An empty vault.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn purge(&self, now: Instant) {
        self.entries
            .borrow_mut()
            .retain(|entry| entry.expires_at > now);
    }

    /// Hold `header` for `origin` and `path`; return its handle.
    ///
    /// # Errors
    /// [`VaultError::Random`] if the OS random source fails.
    pub fn deposit(
        &self,
        header: ProofHeader,
        origin: &str,
        path: &str,
        now: Instant,
    ) -> Result<String, VaultError> {
        self.purge(now);
        let mut bytes = [0u8; 24];
        getrandom::fill(&mut bytes).map_err(|_| VaultError::Random)?;
        let handle = format!("x402h_{}", toolkit_core::encoding::base64url_nopad(&bytes));
        self.entries.borrow_mut().push(Entry {
            handle: handle.clone(),
            header,
            origin: origin.to_owned(),
            path: path.to_owned(),
            expires_at: now + HANDLE_TTL,
        });
        Ok(handle)
    }

    /// Redeem `handle` for one navigation to `url`: the one point where the
    /// proof header leaves the vault, for trusted browser glue only.
    ///
    /// # Errors
    /// [`VaultError`] for an unknown, used or expired handle, or another
    /// origin or path (the handle survives a mismatch, as in Python).
    pub fn attach(&self, handle: &str, url: &str, now: Instant) -> Result<ProofHeader, VaultError> {
        self.purge(now);
        let mut entries = self.entries.borrow_mut();
        let index = entries
            .iter()
            .position(|entry| entry.handle == handle)
            .ok_or(VaultError::Unknown)?;
        let parsed = urlparse(url).map_err(|_| VaultError::Origin)?;
        let origin = parsed.canonical_origin().map_err(|_| VaultError::Origin)?;
        if origin.to_lowercase() != entries[index].origin.to_lowercase() {
            return Err(VaultError::Origin);
        }
        let path = if parsed.path().is_empty() {
            "/"
        } else {
            parsed.path()
        };
        if path != entries[index].path {
            return Err(VaultError::Path);
        }
        Ok(entries.remove(index).header)
    }

    /// Handles currently held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.borrow().len()
    }

    /// Whether no handle is held.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.borrow().is_empty()
    }
}

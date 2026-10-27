//! The structured result of an instrumentation operation (`di_result.py`).
//!
//! `ok` is the STATUS channel (process exit `0` when true, `1` when false);
//! `text` is the PRESENTATION channel printed verbatim. `ok` says whether
//! the operation succeeded, not what lifecycle state a breakpoint is in: a
//! successful `check-status` that reports an ERROR breakpoint is still
//! `ok == true`.

/// The (status, presentation) pair an operation returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpResult {
    /// True when the operation succeeded; maps to exit status `0`/`1`.
    pub ok: bool,
    /// The rendered human-readable message.
    pub text: String,
}

impl OpResult {
    /// A successful result.
    #[must_use]
    pub fn success(text: impl Into<String>) -> Self {
        Self {
            ok: true,
            text: text.into(),
        }
    }

    /// A failed result.
    #[must_use]
    pub fn failure(text: impl Into<String>) -> Self {
        Self {
            ok: false,
            text: text.into(),
        }
    }

    /// The process exit status the entry script derives: `0` or `1`.
    #[must_use]
    pub fn exit_status(&self) -> u8 {
        u8::from(!self.ok)
    }
}

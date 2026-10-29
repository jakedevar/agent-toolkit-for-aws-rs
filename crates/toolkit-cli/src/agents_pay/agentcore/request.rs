use super::{process_body, Approved};

/// Immutable payment request derived solely from an approved selection.
///
/// ```compile_fail
/// use toolkit_cli::agents_pay::{agentcore::ProcessPaymentRequest, policy::Approved};
/// fn forge(approved: &Approved) { let _ = ProcessPaymentRequest::from_approved(approved); }
/// ```
///
/// ```compile_fail
/// use toolkit_cli::agents_pay::agentcore::ProcessPaymentRequest as RequestAlias;
/// fn forge() { let _ = RequestAlias { body: vec![], manager: String::new(), session: String::new(), instrument: String::new(), user: String::new() }; }
/// ```
///
/// ```compile_fail
/// use toolkit_cli::agents_pay::agentcore::ProcessPaymentRequest;
/// fn change(request: &mut ProcessPaymentRequest) { request.body[0] = b'x'; request.manager += ""; }
/// ```
pub struct ProcessPaymentRequest {
    body: Vec<u8>,
    manager: String,
    session: String,
    instrument: String,
    user: String,
}

impl ProcessPaymentRequest {
    pub(super) fn from_approved(approved: &Approved) -> Self {
        Self {
            body: process_body(approved).into_bytes(),
            manager: approved.manager_arn().into(),
            session: approved.session_id().into(),
            instrument: approved.instrument_id().into(),
            user: approved.user_id().into(),
        }
    }

    pub(super) fn body(&self) -> &[u8] {
        &self.body
    }
    pub(super) fn manager(&self) -> &str {
        &self.manager
    }
    pub(super) fn session(&self) -> &str {
        &self.session
    }
    pub(super) fn instrument(&self) -> &str {
        &self.instrument
    }
    pub(super) fn user(&self) -> &str {
        &self.user
    }
}

impl std::fmt::Debug for ProcessPaymentRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ProcessPaymentRequest([REDACTED])")
    }
}

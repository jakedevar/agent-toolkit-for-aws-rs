//! The approved payment: the only value a spending call accepts
//! (R-SPEND-ORDER rules 1-3, x402 S1 #5118).
//!
//! Every field is private to this file and the one constructor is
//! `pub(in crate::agents_pay::policy)`; its only caller is
//! `policy::authorize`, after every check has passed. There is no `&mut self`
//! method, no setter and no `Clone`/`Copy`, so a spending call that takes an
//! `Approved` by value gets exactly the terms the gate approved, once.
//! `tests/agents_pay_gate.rs` (T-ORDER-CTOR) pins these rules for the rest
//! of the crate.

use std::fmt;

use toolkit_core::Json;

use crate::agents_pay::amount::usd_text;

/// The resolved `AgentCore` identifiers the payment is made with. The session
/// id here is the same value that went into the token.
pub struct PaymentIds {
    pub(in crate::agents_pay::policy) manager_arn: String,
    pub(in crate::agents_pay::policy) instrument_id: String,
    pub(in crate::agents_pay::policy) session_id: String,
    pub(in crate::agents_pay::policy) user_id: String,
}

/// A payment the policy gate approved (R-SPEND-ORDER).
///
/// It can only come from [`authorize`](super::authorize), and a spending
/// call consumes it by value:
///
/// ```
/// use toolkit_cli::agents_pay::policy::{authorize, Approved, FixedResolver, PaymentRequest, Resources};
/// use toolkit_core::parse_json;
///
/// fn spend(approved: Approved) -> String {
///     approved.token().to_owned()
/// }
///
/// let policy = parse_json(
///     r#"{"max_per_payment_usd":"0.10","allowed_networks":["eip155:84532"],
///         "allowed_assets":{"eip155:84532":["0xa"]},"allowed_recipients":["0xb"]}"#,
/// )
/// .unwrap();
/// let challenge = parse_json(
///     r#"{"accepts":[{"scheme":"exact","network":"eip155:84532","asset":"0xA",
///         "payTo":"0xB","amount":"100000"}]}"#,
/// )
/// .unwrap();
/// let resources = Resources {
///     payment_manager_arn: Some("arn".into()),
///     payment_instrument_id: Some("pi".into()),
///     payment_session_id: Some("ps".into()),
///     user_id: Some("user".into()),
/// };
/// let request = PaymentRequest {
///     url: "https://93.184.216.34/item",
///     challenge: &challenge,
///     policy: &policy,
///     resources: &resources,
///     purchase_id: None,
/// };
/// let approved = authorize(&request, &FixedResolver::new()).unwrap();
/// assert_eq!(approved.amount(), "100000");
/// assert_eq!(spend(approved).len(), 64);
/// ```
///
/// Outside this crate it cannot be built, changed, copied or spent twice
/// (T-ORDER-CTOR). A struct literal does not compile:
///
/// ```compile_fail,E0451
/// use toolkit_cli::agents_pay::policy::Approved;
///
/// fn forge(token: String) -> Approved {
///     Approved { token, ..todo!() }
/// }
/// ```
///
/// Nor does the constructor:
///
/// ```compile_fail,E0624
/// use toolkit_cli::agents_pay::policy::Approved;
///
/// fn forge() -> Approved {
///     Approved::new(todo!(), todo!(), todo!(), todo!(), todo!(), todo!())
/// }
/// ```
///
/// Nor a field assignment:
///
/// ```compile_fail,E0616
/// use toolkit_cli::agents_pay::policy::Approved;
///
/// fn raise(mut approved: Approved) -> Approved {
///     approved.amount = "999999999".to_owned();
///     approved
/// }
/// ```
///
/// Nor a change through an accessor:
///
/// ```compile_fail,E0594
/// use toolkit_cli::agents_pay::policy::Approved;
/// use toolkit_core::Json;
///
/// fn swap(approved: &mut Approved) {
///     *approved.entry() = Json::Null;
/// }
/// ```
///
/// Nor a copy:
///
/// ```compile_fail,E0277
/// use toolkit_cli::agents_pay::policy::Approved;
///
/// fn copy(approved: &Approved) -> Approved {
///     <Approved as Clone>::clone(approved)
/// }
/// ```
///
/// Nor spending one value twice:
///
/// ```compile_fail,E0382
/// use toolkit_cli::agents_pay::policy::Approved;
///
/// fn spend(_approved: Approved) {}
///
/// fn twice(approved: Approved) {
///     spend(approved);
///     spend(approved);
/// }
/// ```
pub struct Approved {
    entry: Json,
    amount: String,
    version: i64,
    resource: Option<Json>,
    token: String,
    ids: PaymentIds,
}

impl Approved {
    /// Only `policy::authorize` calls this, after every check passed.
    pub(in crate::agents_pay::policy) fn new(
        entry: Json,
        amount: String,
        version: i64,
        resource: Option<Json>,
        token: String,
        ids: PaymentIds,
    ) -> Self {
        Self {
            entry,
            amount,
            version,
            resource,
            token,
            ids,
        }
    }

    /// The vetted entry: the selected `accepts` entry in its original key
    /// order, the aliases removed and the canonical amount appended under
    /// `maxAmountRequired` (version 1) or `amount` (any other version).
    #[must_use]
    pub fn entry(&self) -> &Json {
        &self.entry
    }

    /// The canonical amount in base units, verbatim.
    #[must_use]
    pub fn amount(&self) -> &str {
        &self.amount
    }

    /// The amount in USD as the legacy receipt printed it.
    #[must_use]
    pub fn amount_usd(&self) -> String {
        usd_text(&self.amount)
    }

    /// The x402 version (R-VER).
    #[must_use]
    pub fn version(&self) -> i64 {
        self.version
    }

    /// The sanitized resource `{"url": ...}` (R-RESBIND), if the challenge
    /// had one.
    #[must_use]
    pub fn resource(&self) -> Option<&Json> {
        self.resource.as_ref()
    }

    /// The idempotency token (R-TOK).
    #[must_use]
    pub fn token(&self) -> &str {
        &self.token
    }

    /// The payment manager ARN.
    #[must_use]
    pub fn manager_arn(&self) -> &str {
        &self.ids.manager_arn
    }

    /// The payment instrument id.
    #[must_use]
    pub fn instrument_id(&self) -> &str {
        &self.ids.instrument_id
    }

    /// The payment session id (the same one the token was derived with).
    #[must_use]
    pub fn session_id(&self) -> &str {
        &self.ids.session_id
    }

    /// The payment user id.
    #[must_use]
    pub fn user_id(&self) -> &str {
        &self.ids.user_id
    }
}

impl fmt::Debug for Approved {
    /// Only the token prefix and the amount: no identifier, recipient or
    /// publisher value.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let prefix: String = self.token.chars().take(8).collect();
        f.debug_struct("Approved")
            .field("token", &format_args!("{prefix}..."))
            .field("amount", &self.amount)
            .finish_non_exhaustive()
    }
}

//! x402 payments (#4410), ported from `plugins/aws-agents/skills/agents-pay`.
//!
//! Slice S1 (#5118, design `docs/migration/x402-payments-design.md` section
//! 8) is the pure, network-free payment policy:
//!
//! - [`urlparse`]: Python `urlparse` compatibility (R-URLPARSE);
//! - [`amount`]: challenge amounts, the cap decimal and its comparison, the
//!   `amount_usd` rendering (R-AMT);
//! - [`pyval`]: Python `str()`, truthiness and `int()` over JSON;
//! - [`challenge`]: challenge extraction from a 402 response (R-CH);
//! - [`policy`]: the gate (R-ORIGIN, R-POL, R-VER, R-RESBIND, R-TOK) and the
//!   [`policy::Approved`] value every spending call consumes
//!   (R-SPEND-ORDER).
//!
//! There is no route, configuration file, network or AWS access in this
//! slice; later slices add them around this gate.

pub mod amount;
pub mod challenge;
pub mod policy;
pub mod pyval;
pub mod urlparse;

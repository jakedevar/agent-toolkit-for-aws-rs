//! `toolkit agents-pay bridge`: the `OpenClaw` plugin's trusted glue
//! (#4411, design `docs/migration/openclaw-bridge-design.md` sections 4 and
//! 5.1).
//!
//! One JSON request on stdin, one JSON line on stdout, exit 0/1/2, nothing
//! on stderr. [`serve_frame`] is the pure pipe: the bounded read and B1's
//! validation ([`protocol`]), then `sanitize_output` answered by the pure
//! final scrub ([`output_scrub`], R-OC-FINAL) before any world exists, and
//! every valid service request handed to the build's service. A build
//! without `agents-pay-live` has one service, the pinned R-BR-D32 refusal
//! (`no_payment.rs`); a live build builds the process world and runs
//! [`handle_service`] (`live_command`). Spending goes only through
//! `flow::prove` (R-BR-ONE-1).

pub mod output_scrub;
pub mod protocol;
pub mod render;

#[cfg(not(feature = "agents-pay-live"))]
pub(super) mod no_payment;

use std::io::Read;
use std::time::{Duration, Instant};

use toolkit_core::{Json, ToolkitError};

use self::protocol::BridgeError;
use super::deps::Deps;
use super::flow::{self, ProveError, ProveInput};
use super::policy::Resources;
use crate::safe_text::KnownSecrets;
use crate::Output;

/// One complete output frame (bytes, final LF included) and its exit code.
pub type Frame = (Vec<u8>, i32);

/// R-BR-D32: the whole stdout of a valid service-operation request in a
/// build without `agents-pay-live`.
pub const UNAVAILABLE_FRAME: &[u8] = b"{\"ok\":false,\"error\":\"payment support unavailable\"}\n";

/// R-BR-DL: the bridge's operation deadline, 5 s inside `bridge.ts`'s 30 s.
pub const DEADLINE: Duration = Duration::from_secs(25);

/// The pure pipe: read and validate one request, answer `sanitize_output`
/// without any world, and hand a valid service operation to `service`. A
/// panic anywhere is the `AgentCore` failure (R-BR-QUIET).
#[must_use]
pub fn serve_frame(reader: &mut dyn Read, service: &dyn Fn(&Json) -> Frame) -> Frame {
    let outcome =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            || match protocol::read_request(&mut &mut *reader) {
                Err(error) => render::error_frame(error),
                Ok(request) => {
                    if request.get("operation").and_then(Json::as_str) == Some("sanitize_output") {
                        output_scrub::sanitize_frame(&request)
                    } else {
                        service(&request)
                    }
                }
            },
        ));
    outcome.unwrap_or_else(|_| render::error_frame(BridgeError::AgentCore))
}

/// The registry entries' pipe: a silent panic hook (R-BR-QUIET), then
/// [`serve_quiet`].
///
/// # Errors
/// None: see [`serve_quiet`].
pub(in crate::agents_pay) fn serve(
    reader: &mut dyn Read,
    out: &mut Output<'_>,
    service: &dyn Fn(&Json) -> Frame,
) -> Result<(), ToolkitError> {
    std::panic::set_hook(Box::new(|_| {}));
    serve_quiet(reader, out, service)
}

/// [`serve_frame`], then the frame on stdout and its exit status, with
/// nothing on stderr.
///
/// # Errors
/// None: a stdout write failure becomes exit 1 rather than an error
/// report on stderr.
pub fn serve_quiet(
    reader: &mut dyn Read,
    out: &mut Output<'_>,
    service: &dyn Fn(&Json) -> Frame,
) -> Result<(), ToolkitError> {
    let (bytes, status) = serve_frame(reader, service);
    let line = bytes.strip_suffix(b"\n").unwrap_or(&bytes);
    let written = std::str::from_utf8(line)
        .map_err(|_| ())
        .and_then(|text| out.line(text).map_err(|_| ()));
    out.set_exit_status(match written {
        Ok(()) => u8::try_from(status).unwrap_or(1),
        Err(()) => 1,
    });
    Ok(())
}

/// The process stdin, unbuffered (R-BR-IN): a duplicate of the standard
/// input descriptor, sharing its file offset, read directly. `io::Stdin`
/// reads ahead in 8 KiB blocks, which would consume up to 139264 bytes of a
/// longer stream; this reader takes at most the 131073 the bounded read asks
/// for. A descriptor that cannot be duplicated reads as empty (an invalid
/// request).
#[must_use]
pub(in crate::agents_pay) fn process_stdin() -> ::std::boxed::Box<dyn ::std::io::Read> {
    #[cfg(unix)]
    let owned = ::std::os::fd::AsFd::as_fd(&::std::io::stdin()).try_clone_to_owned();
    #[cfg(windows)]
    let owned =
        ::std::os::windows::io::AsHandle::as_handle(&::std::io::stdin()).try_clone_to_owned();
    match owned {
        ::core::result::Result::Ok(owned) => ::std::boxed::Box::new(::std::fs::File::from(owned)),
        ::core::result::Result::Err(_) => ::std::boxed::Box::new(::std::io::empty()),
    }
}

/// The live registry entry: the process world is built only for a valid
/// service request, after the pure route (R-BR-WORLD).
///
/// # Errors
/// None: see [`serve_quiet`].
#[cfg(feature = "agents-pay-live")]
pub(in crate::agents_pay) fn live_command(
    _invocation: &crate::Invocation,
    out: &mut crate::Output<'_>,
) -> ::core::result::Result<(), ::toolkit_core::ToolkitError> {
    self::serve(
        &mut self::process_stdin(),
        out,
        &|request| match super::deps::Deps::for_process() {
            ::core::result::Result::Ok(world) => self::handle_service(&world.deps(), request),
            ::core::result::Result::Err(_) => {
                self::render::error_frame(self::protocol::BridgeError::AgentCore)
            }
        },
    )
}

fn text<'r>(request: &'r Json, key: &str) -> Result<&'r str, BridgeError> {
    request
        .get(key)
        .and_then(Json::as_str)
        .ok_or(BridgeError::Input)
}

/// The two service operations over an injected world (R-BR-FLOW): a request
/// [`protocol::read_request`] accepted, under the 25 s deadline (R-BR-DL).
/// The bridge reads only `payments`, `resolver` and `clock` (R-BR-WORLD) and
/// never sleeps.
#[must_use]
pub fn handle_service(deps: &Deps<'_>, request: &Json) -> Frame {
    let deadline = deps.clock.now() + DEADLINE;
    let outcome = match request.get("operation").and_then(Json::as_str) {
        Some("get_payment_session") => session(deps, request, deadline),
        Some("process_payment") => payment(deps, request, deadline),
        _ => Err(BridgeError::Input),
    };
    match outcome {
        Ok(frame) => frame,
        Err(error) => render::error_frame(error),
    }
}

/// R-BR-SCRUB over a rendered document (or part of one): a piece of a live
/// secret anywhere in it fails the call.
fn scrubbed(document: &Json, secrets: &KnownSecrets) -> Result<(), BridgeError> {
    if secrets.found_in(&crate::aurora::pyfmt::dumps_compact(document)) {
        Err(BridgeError::AgentCore)
    } else {
        Ok(())
    }
}

/// `value` with member `key` removed from the object at `path`.
fn without(value: &Json, path: &[&str], key: &str) -> Json {
    match (value, path.split_first()) {
        (Json::Object(members), None) => Json::Object(
            members
                .iter()
                .filter(|(name, _)| name != key)
                .cloned()
                .collect(),
        ),
        (Json::Object(members), Some((head, rest))) => Json::Object(
            members
                .iter()
                .map(|(name, member)| {
                    let member = if name == head {
                        without(member, rest, key)
                    } else {
                        member.clone()
                    };
                    (name.clone(), member)
                })
                .collect(),
        ),
        _ => value.clone(),
    }
}

/// The session id's secret part (without the public `payment-session-`
/// head), in every spelling R-SCRUB knows.
fn session_secrets(session: &str) -> KnownSecrets {
    let mut secrets = KnownSecrets::default();
    secrets.learn(flow::session_secret(session));
    secrets
}

fn session(deps: &Deps<'_>, request: &Json, deadline: Instant) -> Result<Frame, BridgeError> {
    let region = text(request, "region")?;
    let manager = text(request, "payment_manager_arn")?;
    let session = text(request, "payment_session_id")?;
    let user = text(request, "user_id")?;
    let api = deps
        .payments
        .open(region, manager, deadline)
        .map_err(|_| BridgeError::Input)?;
    let value = api
        .get_session(manager, session, user)
        .map_err(|_| BridgeError::AgentCore)?;
    let document =
        render::session_result(value.get("paymentSession").ok_or(BridgeError::AgentCore)?)?;
    // The session id is exempt only as `paymentSessionId`.
    let checked = without(&document, &["result", "paymentSession"], "paymentSessionId");
    scrubbed(&checked, &session_secrets(session))?;
    Ok(render::frame(&document))
}

fn payment(deps: &Deps<'_>, request: &Json, deadline: Instant) -> Result<Frame, BridgeError> {
    let member = |key: &str| request.get(key).cloned().ok_or(BridgeError::Input);
    let policy = super::policy::openclaw::from_openclaw(&member("policy")?)?;
    let challenge = Json::object(vec![
        ("x402Version", Json::Int(2)),
        ("accepts", Json::Array(vec![member("accepted")?])),
        ("resource", member("resource")?),
    ]);
    let session = text(request, "payment_session_id")?;
    let resources = Resources {
        payment_manager_arn: Some(text(request, "payment_manager_arn")?.to_owned()),
        payment_instrument_id: Some(text(request, "payment_instrument_id")?.to_owned()),
        payment_session_id: Some(session.to_owned()),
        user_id: Some(text(request, "user_id")?.to_owned()),
    };
    let region = text(request, "region")?.to_owned();
    let input = ProveInput {
        url: text(request, "request_url")?,
        challenge: &challenge,
        policy: &policy,
        resources: &resources,
        purchase_id: None,
        deadline,
    };
    let (verified, _wait) =
        flow::prove(deps, &input, |_| Ok(region)).map_err(|error| match error {
            ProveError::Refused(_) => BridgeError::Input,
            ProveError::Payment(_) | ProveError::Proof(_) => BridgeError::AgentCore,
        })?;
    let mut proof_secrets = session_secrets(session);
    proof_secrets.learn(verified.header().value());
    if let Some(signature) = verified.signature() {
        proof_secrets.learn(signature);
    }
    let document = render::glue_result(verified)?;
    // The designated payload path is exempt from the proof's own match and
    // nowhere else; the session id is matched everywhere.
    let payload = document
        .get("result")
        .and_then(|r| r.get("paymentOutput"))
        .and_then(|r| r.get("cryptoX402"))
        .and_then(|r| r.get("payload"))
        .ok_or(BridgeError::AgentCore)?;
    scrubbed(payload, &session_secrets(session))?;
    let outside = without(
        &document,
        &["result", "paymentOutput", "cryptoX402"],
        "payload",
    );
    scrubbed(&outside, &proof_secrets)?;
    Ok(render::frame(&document))
}

#[cfg(test)]
mod scrub_tests;
#[cfg(test)]
mod tests;

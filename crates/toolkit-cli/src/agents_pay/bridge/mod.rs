//! `toolkit agents-pay bridge`: the `OpenClaw` plugin's trusted glue.

pub mod output_scrub;
pub mod protocol;
pub mod render;

use std::io::Read;
use std::time::Duration;

use toolkit_core::{Json, ToolkitError};

use self::protocol::BridgeError;
use super::deps::Deps;
use crate::Output;

/// One complete output frame (bytes, final LF included) and its exit code.
pub type Frame = (Vec<u8>, i32);

/// R-BR-D32: the whole stdout of a valid service-operation request in a
/// build without `agents-pay-live`.
pub const UNAVAILABLE_FRAME: &[u8] = b"{\"ok\":false,\"error\":\"payment support unavailable\"}\n";

/// R-BR-DL: the bridge's operation deadline, 5 s inside `bridge.ts`'s 30 s.
pub const DEADLINE: Duration = Duration::from_secs(25);

/// The pure pipe: read and validate one request, answer `sanitize_output`
/// without any world, and hand a service operation to `service`.
#[must_use]
pub fn serve_frame(_reader: &mut dyn Read, _service: &dyn Fn(&Json) -> Frame) -> Frame {
    render::error_frame(BridgeError::AgentCore)
}

/// [`serve_frame`], then the frame on stdout and its exit status, with
/// nothing on stderr.
///
/// # Errors
/// None: a stdout failure becomes exit 1.
pub fn serve_quiet(
    reader: &mut dyn Read,
    out: &mut Output<'_>,
    service: &dyn Fn(&Json) -> Frame,
) -> Result<(), ToolkitError> {
    let (_bytes, _status) = serve_frame(reader, service);
    let _ = out;
    Ok(())
}

/// The two service operations over an injected world.
#[must_use]
pub fn handle_service(_deps: &Deps<'_>, _request: &Json) -> Frame {
    render::error_frame(BridgeError::AgentCore)
}

#[cfg(test)]
mod scrub_tests;
#[cfg(test)]
mod tests;

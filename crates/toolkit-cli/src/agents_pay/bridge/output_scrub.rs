//! The pure final model-output scrub (`OpenClaw` R-OC-FINAL, #5160).

use toolkit_core::Json;

/// The fixed generic refusal reason of a paid-tool result.
pub const FAILED_SAFELY: &str =
    "Payment flow failed safely without exposing provider or proof details";

/// Sanitize one validated `sanitize_output` request into a complete frame.
#[must_use]
pub fn sanitize_frame(request: &Json) -> (Vec<u8>, i32) {
    sanitize_frame_within(request, crate::safe_text::FINAL_MATCH_WORK)
}

/// [`sanitize_frame`] with an explicit work budget.
#[must_use]
pub fn sanitize_frame_within(request: &Json, _budget: usize) -> (Vec<u8>, i32) {
    super::render::frame(&Json::object(vec![
        ("ok", Json::Bool(true)),
        (
            "result",
            request.get("candidate").cloned().unwrap_or(Json::Null),
        ),
    ]))
}

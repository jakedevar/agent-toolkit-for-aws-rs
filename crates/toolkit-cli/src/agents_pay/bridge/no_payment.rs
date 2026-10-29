// The `agents-pay bridge` route of a build without `agents-pay-live`
// (`OpenClaw` B2, #5443; R-BR-D32 and the G1c refinement of design 4.8).
// The registry calls `bridge_command`, which hands the process stdin to the
// pure pipe `super::serve` with `refuse_without_payment_support` as the only
// service: `sanitize_output` is answered without a world, every other valid
// request gets the fixed `UNAVAILABLE_FRAME` literal and exit 1. No world is
// built, and no file, variable, credential or terminal is read.
//
// As in `agents_pay/no_payment.rs`, every path here is absolute or
// `self`/`super`, there is no `use`, no macro invocation and no other item,
// and `tests/agents_pay_gate.rs` pins this file's whole token stream. Only
// `//` comments are allowed here.

pub(in crate::agents_pay) fn bridge_command(
    _invocation: &crate::Invocation,
    out: &mut crate::Output<'_>,
) -> ::core::result::Result<(), ::toolkit_core::ToolkitError> {
    super::serve(
        &mut super::process_stdin(),
        out,
        &self::refuse_without_payment_support,
    )
}

fn refuse_without_payment_support(_request: &::toolkit_core::Json) -> (::std::vec::Vec<u8>, i32) {
    (::std::vec::Vec::from(super::UNAVAILABLE_FRAME), 1)
}

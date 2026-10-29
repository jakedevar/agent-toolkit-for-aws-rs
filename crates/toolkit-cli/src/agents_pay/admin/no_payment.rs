// Every admin route of a build without `agents-pay-live` (D32, x402 S6
// #5462): `agents-pay admin *` and `agents-build setup-payment-user`. The
// registry calls `admin_command`, which makes one call to
// `refuse_without_payment_support`, which prints the fixed
// `D32_ADMIN_REFUSAL` literal on stderr and sets exit 1. No world is built,
// and no file, variable, credential, deployed state or terminal is read.
//
// Every path in this file is absolute (`::core`, `::toolkit_core`, `crate`,
// `self`, `super`), and the file has no `use`, no macro invocation and no
// other item, so no module item, import or textual macro can shadow a name it
// resolves. `tests/agents_pay_gate.rs` pins this file's whole token stream:
// any change to it fails the gate. Only `//` comments are allowed here, as
// they are not tokens; a doc comment would be.

pub(super) fn admin_command(
    _invocation: &crate::Invocation,
    out: &mut crate::Output<'_>,
) -> ::core::result::Result<(), ::toolkit_core::ToolkitError> {
    self::refuse_without_payment_support(out)
}

fn refuse_without_payment_support(
    out: &mut crate::Output<'_>,
) -> ::core::result::Result<(), ::toolkit_core::ToolkitError> {
    out.diagnostic(super::D32_ADMIN_REFUSAL);
    out.set_exit_status(1);
    ::core::result::Result::Ok(())
}

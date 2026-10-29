//! `secret-safety` hook binary: JSON on stdin, optional deny object on stdout.

use std::io::{self, Write};
use std::process::ExitCode;

use secret_safety_hook::{decide_from_reader, DEFAULT_DEADLINE};

fn main() -> ExitCode {
    let decision = decide_from_reader(io::stdin(), DEFAULT_DEADLINE);
    let output = decision.render();
    let mut stdout = io::stdout().lock();
    // A verdict that cannot be delivered must not look like an allow: exit
    // non-zero so the launcher substitutes its own deny.
    if stdout.write_all(output.as_bytes()).is_err() || stdout.flush().is_err() {
        return ExitCode::from(2);
    }
    ExitCode::SUCCESS
}

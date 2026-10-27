//! `toolkit` — read-only utility CLI for the Agent Toolkit fork.
//!
//! The command contract (commands, flags, exit codes, JSON shape) is in
//! `crates/toolkit-cli/README.md`; routing and output handling are in the
//! `toolkit_cli` library so they can be tested without spawning a process.

use std::ffi::OsString;
use std::io;
use std::process::ExitCode;

fn main() -> ExitCode {
    // args_os, not args(): a non-UTF-8 argument must be a reported usage
    // error (exit 2), never a panic.
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let stdout = io::stdout();
    let stderr = io::stderr();
    let mut out = stdout.lock();
    let mut err = stderr.lock();
    ExitCode::from(toolkit_cli::run(&args, &mut out, &mut err))
}

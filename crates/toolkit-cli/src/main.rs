//! `toolkit` — read-only utility CLI for the Agent Toolkit fork.
//!
//! The command contract (commands, flags, exit codes, JSON shape) is in
//! `crates/toolkit-cli/README.md`; routing and output handling are in the
//! `toolkit_cli` library so they can be tested without spawning a process.

use std::ffi::OsString;
use std::io::{self, Write};
use std::process::ExitCode;

fn main() -> ExitCode {
    // args_os, not args(): a non-UTF-8 argument must be a reported usage
    // error (exit 2), never a panic.
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let stdout = io::stdout();
    let stderr = io::stderr();
    let mut out = stdout.lock();
    let mut err = stderr.lock();
    #[cfg(unix)]
    if args == ["launch", "auth-callback"] {
        let mut announcement = CallbackOutput {
            inner: &mut out,
            written: false,
        };
        return ExitCode::from(toolkit_cli::run(&args, &mut announcement, &mut err));
    }
    ExitCode::from(toolkit_cli::run(&args, &mut out, &mut err))
}

#[cfg(unix)]
struct CallbackOutput<'a, W> {
    inner: &'a mut W,
    written: bool,
}

#[cfg(unix)]
impl<W: Write> Write for CallbackOutput<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(bytes)?;
        self.written |= n != 0;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()?;
        if self.written {
            let null = std::fs::OpenOptions::new().write(true).open("/dev/null")?;
            rustix::stdio::dup2_stdout(null)?;
            self.written = false;
        }
        Ok(())
    }
}

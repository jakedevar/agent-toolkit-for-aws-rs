//! Running the resolved command.
//!
//! The child gets the resolved arguments on its command line and the resolved
//! environment (nothing else: no stdin is written, no file is created) and
//! shares this process's stdin, stdout and stderr, as `subprocess.run` did.
//! A secret is therefore visible to the child (and, for the arguments, to the
//! process table while the child runs) and to nobody else: it is never printed
//! by `asm exec`.

use std::ffi::OsString;
use std::io;
use std::process::Command;

/// How a command ended, as the exit status `asm exec` returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExitCode(pub u8);

/// Starts a program and waits for it.
pub trait Launcher {
    /// Run `program` with `args` and exactly `env` as its environment, sharing
    /// the standard streams, and report its exit status.
    ///
    /// # Errors
    /// The operating-system error when the program cannot be started.
    fn run(
        &self,
        program: &OsString,
        args: &[OsString],
        env: &[(OsString, OsString)],
    ) -> io::Result<ExitCode>;
}

/// The real launcher.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemLauncher;

impl Launcher for SystemLauncher {
    fn run(
        &self,
        program: &OsString,
        args: &[OsString],
        env: &[(OsString, OsString)],
    ) -> io::Result<ExitCode> {
        let status = Command::new(program)
            .args(args)
            .env_clear()
            .envs(env.iter().map(|(name, value)| (name, value)))
            .status()?;
        Ok(exit_code(status))
    }
}

/// The Python's `sys.exit(returncode)`: the exit code, or for a child ended by
/// signal N the value `256 - N` (what `sys.exit(-N)` becomes).
#[must_use]
pub fn exit_code(status: std::process::ExitStatus) -> ExitCode {
    if let Some(code) = status.code() {
        return ExitCode(u8::try_from(code & 0xff).unwrap_or(1));
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return ExitCode(
                256_i32
                    .checked_sub(signal)
                    .and_then(|value| u8::try_from(value).ok())
                    .unwrap_or(1),
            );
        }
    }
    ExitCode(1)
}

#[cfg(test)]
mod tests;

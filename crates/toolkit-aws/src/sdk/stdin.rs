//! D2: `credential_process` children get a null stdin (#4413 S1, #4994).
//!
//! aws-config runs `credential_process` with tokio's `Command::output`, which
//! pipes only stdout and stderr: the child inherits the proxy's stdin, which
//! is the MCP client's message stream. [`isolate_stdin`] moves that stream to a
//! private descriptor and puts the null device on fd 0, so **every** later
//! child (credential processes and anything else) reads EOF at once, and the
//! MCP reader keeps the original stream through the returned [`File`].
//!
//! It must run at startup, before anything reads `std::io::stdin()` (whose
//! buffer would otherwise hold bytes the private descriptor no longer sees)
//! and before any child is spawned. The `dup`/`dup2` calls go through
//! `rustix`'s safe wrappers; the workspace forbids `unsafe`.

use std::fs::File;

use crate::error::AwsError;

/// Move the process's stdin to a private descriptor, put the null device on
/// fd 0, and return the original stream.
///
/// # Errors
/// `InvalidConfig` (field `stdin`) when a descriptor operation fails, or on a
/// platform without the mechanism (non-Unix).
#[cfg(unix)]
pub fn isolate_stdin() -> Result<File, AwsError> {
    use std::os::fd::AsFd;

    let failed = |stage: &str| {
        AwsError::invalid_config(
            "stdin",
            format!("stdin could not be isolated from child processes ({stage})"),
        )
    };
    let original = rustix::io::dup(std::io::stdin().as_fd()).map_err(|_| failed("dup"))?;
    let null = File::open("/dev/null").map_err(|_| failed("open null device"))?;
    rustix::stdio::dup2_stdin(&null).map_err(|_| failed("dup2"))?;
    Ok(File::from(original))
}

/// See the Unix version: there is no safe equivalent here yet, so callers fail
/// closed rather than let a credential process read the MCP stream.
///
/// # Errors
/// Always `InvalidConfig` (field `stdin`).
#[cfg(not(unix))]
pub fn isolate_stdin() -> Result<File, AwsError> {
    Err(AwsError::invalid_config(
        "stdin",
        "stdin cannot be isolated from child processes on this platform",
    ))
}

#[cfg(all(test, unix))]
mod tests {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::process::Stdio;
    use std::sync::mpsc;
    use std::time::Duration;

    use aws_credential_types::provider::ProvideCredentials;

    use super::*;
    use crate::sdk::test_support::{child_case, child_command, report, TempDir, CASE_VAR};
    use crate::sdk::SdkRuntime;

    const ENTRY: &str = "sdk::stdin::tests::child_entry";
    const FIXTURE: &str = "sdk::stdin::tests::fixture_entry";
    const MCP_LINE: &str = "MCP-STDIN-CANARY";

    /// The Rust `credential_process` fixture: read stdin to the end and record
    /// whether that happened at once, then fail (exit 1).
    #[test]
    fn fixture_entry() {
        if std::env::var("TOOLKIT_AWS_FIXTURE").as_deref() != Ok("stdin-probe") {
            return;
        }
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let read = std::io::stdin().read_to_end(&mut bytes);
            let _ = sender.send((read.is_ok(), bytes.len()));
        });
        let outcome = match receiver.recv_timeout(Duration::from_secs(3)) {
            Ok((true, length)) => format!("eof:{length}"),
            Ok((false, _)) => "error".to_string(),
            Err(_) => "blocked".to_string(),
        };
        let path = std::env::var("FIXTURE_RESULT").expect("result path");
        std::fs::write(path, outcome).expect("fixture result");
        std::process::exit(1);
    }

    /// The child: optionally isolate stdin, then run a real aws-config
    /// `credential_process` provider whose command is the fixture.
    #[test]
    fn child_entry() {
        let Some(case) = child_case() else {
            return;
        };
        if case == "isolated" {
            let mcp = isolate_stdin().expect("isolated");
            let mut line = String::new();
            BufReader::new(mcp).read_line(&mut line).expect("mcp line");
            report("mcp_line", line.trim_end());
        }
        let exe = std::env::current_exe().expect("test binary");
        let result = std::env::var("FIXTURE_RESULT").expect("result path");
        let command = format!(
            "TOOLKIT_AWS_FIXTURE=stdin-probe FIXTURE_RESULT='{result}' '{}' --exact {FIXTURE} --nocapture",
            exe.display()
        );
        let runtime = SdkRuntime::new().expect("runtime");
        let provider = aws_config::credential_process::CredentialProcessProvider::new(command);
        let loaded = runtime.block_on(provider.provide_credentials());
        assert!(loaded.is_err(), "the fixture exits 1");
        report(
            "probe",
            std::fs::read_to_string(result).unwrap_or_else(|_| "missing".to_string()),
        );
    }

    /// Run the child with stdin a pipe that stays open, holding one MCP line.
    fn run_with_open_stdin(case: &str) -> (String, String) {
        let tmp = TempDir::new("stdin");
        let result = tmp.path().join("probe");
        let mut command = child_command(ENTRY, case);
        command
            .env("FIXTURE_RESULT", &result)
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().expect("child");
        let mut stdin = child.stdin.take().expect("stdin pipe");
        writeln!(stdin, "{MCP_LINE}").expect("mcp line");
        stdin.flush().expect("flush");
        let output = child.wait_with_output().expect("child output");
        drop(stdin);
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        assert!(
            output.status.success(),
            "{stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let fact = |key: &str| {
            stdout
                .lines()
                .find_map(|line| line.strip_prefix(&format!("RESULT {key}=")))
                .unwrap_or("")
                .to_string()
        };
        (fact("mcp_line"), fact("probe"))
    }

    #[test]
    fn t_cred_4_a_credential_process_child_reads_eof_at_once() {
        let (mcp_line, probe) = run_with_open_stdin("isolated");
        // The MCP reader keeps the original stream...
        assert_eq!(mcp_line, MCP_LINE);
        // ...and the credential process sees an empty, closed stdin.
        assert_eq!(probe, "eof:0");
    }

    #[test]
    fn t_cred_4_control_without_isolation_the_child_inherits_the_mcp_stream() {
        // Proves the case above can fail: the fixture blocks on the open pipe.
        let (_, probe) = run_with_open_stdin("inherited");
        assert_eq!(probe, "blocked");
        assert!(!CASE_VAR.is_empty());
    }
}

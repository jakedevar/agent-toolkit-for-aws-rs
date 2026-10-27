//! Offline tests for exit-status folding and the real launcher (which starts
//! `/bin/sh`, a local program; no network is involved).

use super::*;

#[cfg(unix)]
fn status_of(script: &str) -> std::process::ExitStatus {
    Command::new("/bin/sh")
        .args(["-c", script])
        .status()
        .expect("sh runs")
}

#[cfg(unix)]
#[test]
fn exit_codes_are_forwarded_and_signals_fold_like_sys_exit() {
    assert_eq!(exit_code(status_of("exit 0")), ExitCode(0));
    assert_eq!(exit_code(status_of("exit 7")), ExitCode(7));
    assert_eq!(exit_code(status_of("exit 255")), ExitCode(255));
    // Killed by SIGKILL (9): Python's returncode is -9 and sys.exit(-9) is 247.
    assert_eq!(exit_code(status_of("kill -9 $$")), ExitCode(247));
    // SIGTERM (15) is 241.
    assert_eq!(exit_code(status_of("kill -15 $$")), ExitCode(241));
}

#[cfg(unix)]
#[test]
fn the_child_gets_exactly_the_given_environment_and_arguments() {
    let launcher = SystemLauncher;
    let sh = OsString::from("/bin/sh");
    let args = |script: &str| {
        vec![
            OsString::from("-c"),
            OsString::from(script),
            OsString::from("zero"),
            OsString::from("a b"),
        ]
    };
    let env = vec![
        (OsString::from("PATH"), OsString::from("/usr/bin:/bin")),
        (OsString::from("ASM_TEST_VALUE"), OsString::from("v1")),
    ];
    // Argument 1 is "a b", the variable is set, an inherited one is not.
    let script =
        r#"test "$1" = "a b" && test "$ASM_TEST_VALUE" = v1 && test -z "${HOME+set}" && exit 3"#;
    let code = launcher.run(&sh, &args(script), &env).unwrap();
    assert_eq!(code, ExitCode(3));
}

#[cfg(unix)]
#[test]
fn a_missing_program_is_an_error_not_a_status() {
    let launcher = SystemLauncher;
    let error = launcher
        .run(
            &OsString::from("/nonexistent/asm-exec-test-program"),
            &[],
            &[],
        )
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::NotFound);
}

#[cfg(unix)]
#[test]
fn a_nul_byte_in_an_argument_cannot_start_a_program() {
    use std::os::unix::ffi::OsStringExt;
    let launcher = SystemLauncher;
    let error = launcher
        .run(
            &OsString::from("/bin/true"),
            &[OsString::from_vec(b"a\0b".to_vec())],
            &[],
        )
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
}

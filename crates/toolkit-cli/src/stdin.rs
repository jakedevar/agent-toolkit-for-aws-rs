//! Bounded standard-input reads for the few parameters whose Python original
//! reads a lone `-` as stdin (`STDIO_DASH`, `docs/migration/cli-bare-dash.md`).

use std::io::{self, Read};

use toolkit_core::ToolkitError;

use crate::safe_text::safe_text;

/// The most bytes read from stdin for one argument (1 MiB).
pub(crate) const STDIN_MAX_BYTES: usize = 1024 * 1024;

/// Reads the whole of `reader`, at most [`STDIN_MAX_BYTES`], as UTF-8.
///
/// More bytes, invalid UTF-8 and a read error are usage errors (exit 2, the
/// `ap.error` status of the Python originals). `flag` names the parameter in the
/// message and is always a registered flag name.
pub(crate) fn read_bounded(reader: &mut dyn Read, flag: &str) -> Result<String, ToolkitError> {
    let mut bytes = Vec::new();
    let limit = (STDIN_MAX_BYTES as u64) + 1;
    reader
        .take(limit)
        .read_to_end(&mut bytes)
        .map_err(|error| unreadable(flag, &safe_text(&error.to_string())))?;
    if bytes.len() > STDIN_MAX_BYTES {
        return Err(unreadable(
            flag,
            &format!("stdin is larger than {STDIN_MAX_BYTES} bytes"),
        ));
    }
    String::from_utf8(bytes).map_err(|_| unreadable(flag, "stdin is not valid UTF-8"))
}

/// [`read_bounded`] over the process's standard input.
pub(crate) fn read_stdin(flag: &str) -> Result<String, ToolkitError> {
    read_bounded(&mut io::stdin().lock(), flag)
}

fn unreadable(flag: &str, why: &str) -> ToolkitError {
    ToolkitError::usage(
        "arguments_unreadable",
        format!("{flag} - could not be read: {why}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_up_to_the_cap_and_refuses_one_byte_more() {
        let at_cap = vec![b' '; STDIN_MAX_BYTES];
        let text = read_bounded(&mut at_cap.as_slice(), "--json").expect("at the cap");
        assert_eq!(text.len(), STDIN_MAX_BYTES);
        let over = vec![b' '; STDIN_MAX_BYTES + 1];
        let error = read_bounded(&mut over.as_slice(), "--json").expect_err("over the cap");
        assert_eq!(error.exit_code(), 2);
        assert!(error.message().contains("1048576"), "{}", error.message());
    }

    struct Broken;

    impl Read for Broken {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::other("boom"))
        }
    }

    #[test]
    fn refuses_invalid_utf8_and_reports_read_errors() {
        let error = read_bounded(&mut b"\xff".as_slice(), "--json-file").expect_err("utf-8");
        assert!(error.message().contains("UTF-8"), "{}", error.message());
        assert!(error
            .message()
            .starts_with("--json-file - could not be read"));

        let error = read_bounded(&mut Broken, "--json").expect_err("read error");
        assert_eq!(error.exit_code(), 2);
        assert!(error.message().contains("boom"));
    }
}

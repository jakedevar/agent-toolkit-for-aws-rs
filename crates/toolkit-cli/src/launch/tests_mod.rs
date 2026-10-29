//! T-AR-OPEN-9 (the platform switch) and the message helpers.

use super::text::{repr, shown};
use super::*;

#[test]
fn t_ar_open_9_non_unix_dispatch_returns_the_platform_message() {
    assert_eq!(platform_gate(true), Ok(()));
    assert_eq!(platform_gate(false), Err(LaunchError::Platform));
    assert_eq!(
        LaunchError::Platform.to_string(),
        "This command is not supported on this platform."
    );
    assert_eq!(require_archive_platform().is_ok(), cfg!(unix));
}

#[test]
fn repr_follows_python_quoting_and_escapes_unsafe_characters() {
    assert_eq!(repr("plain"), "'plain'");
    assert_eq!(repr("it's"), "\"it's\"");
    assert_eq!(repr("both ' and \""), "'both \\' and \"'");
    assert_eq!(repr("a\\b"), "'a\\\\b'");
    assert_eq!(repr("tab\tnew\nret\r"), "'tab\\tnew\\nret\\r'");
    assert_eq!(repr("\u{1b}[31m\u{7f}\u{85}"), "'\\x1b[31m\\x7f\\x85'");
    assert_eq!(repr("bidi\u{202e}x\u{e0001}"), "'bidi\\u202ex\\U000e0001'");
    assert_eq!(repr("caf\u{e9} \u{65e5}"), "'caf\u{e9} \u{65e5}'");
    let secret = repr("https://user:FAKE_SECRET@host.example/x");
    assert!(!secret.contains("FAKE_SECRET"), "{secret}");
    let long = repr(&"x".repeat(1000));
    assert!(long.len() < 260 && long.ends_with("...'"), "{long}");
    assert_eq!(shown("a\u{200c}b"), "a\\u200cb");
    assert_eq!(shown("it's"), "it's");
}

#[test]
fn errors_display_their_message_and_debug_without_raw_values() {
    let error = LaunchError::Config("Invalid X: host 'h' is not allowed.".to_string());
    assert_eq!(error.to_string(), "Invalid X: host 'h' is not allowed.");
    assert_eq!(error.message(), "Invalid X: host 'h' is not allowed.");
    assert!(format!("{error:?}").starts_with("LaunchError::Config("));
}

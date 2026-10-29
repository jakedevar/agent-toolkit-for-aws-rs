//! Linear-time scanners that replace the regular expressions of the legacy
//! Python hook (`secret-safety.py`).
//!
//! The legacy patterns are backtracking regexes; one of them needed a comment
//! explaining how to keep it linear because a hang past the host timeout would
//! let the call through. These scanners tokenise once and never backtrack over
//! unbounded input: the only repetition is the documented cap of twelve flag
//! groups, and every scan position does constant work apart from skipping a
//! whitespace or letter run that no other position rescans.
//!
//! Semantics follow Python's `str` patterns: `\s` is [`is_space`], and
//! case-insensitive matching uses [`fold`]. Deliberate differences are only
//! ever stricter and are listed in `docs/migration/security-hooks-review.md`.

/// Python `str.isspace`, which is what `\s` and `str.split()` use for `str`.
pub(crate) fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// Simple case folding used for case-insensitive matching. Besides lower
/// casing it folds the characters Python's `re.IGNORECASE` also equates with
/// ASCII letters (long s, dotted/dotless i, Kelvin sign).
pub(crate) fn fold(c: char) -> char {
    match c {
        '\u{17f}' => 's',
        '\u{130}' | '\u{131}' => 'i',
        '\u{212a}' => 'k',
        _ => {
            let mut lower = c.to_lowercase();
            match (lower.next(), lower.next()) {
                (Some(single), None) => single,
                _ => c,
            }
        }
    }
}

fn folded(text: &str) -> Vec<char> {
    text.chars().map(fold).collect()
}

fn at(chars: &[char], index: usize) -> Option<char> {
    chars.get(index).copied()
}

/// True when `chars[index..]` starts with the ASCII literal `literal`.
fn starts_at(chars: &[char], index: usize, literal: &str) -> bool {
    let mut position = index;
    for expected in literal.chars() {
        if at(chars, position) != Some(expected) {
            return false;
        }
        position += 1;
    }
    true
}

fn skip_spaces(chars: &[char], index: usize) -> usize {
    let mut position = index;
    while at(chars, position).is_some_and(is_space) {
        position += 1;
    }
    position
}

/// Python `\w` restricted to ASCII. A following non-ASCII letter therefore
/// counts as a word boundary, which can only make a match more likely.
fn is_word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// True when the folded text contains the ASCII literal, ignoring case.
pub(crate) fn contains_folded(text: &str, literal: &str) -> bool {
    let chars = folded(text);
    let needle: Vec<char> = literal.chars().collect();
    if needle.is_empty() || chars.len() < needle.len() {
        return false;
    }
    chars
        .windows(needle.len())
        .any(|window| window == needle.as_slice())
}

// ---------------------------------------------------------------------------
// AWS CLI shape
// ---------------------------------------------------------------------------

/// Maximum flag groups allowed between the words of the CLI shape.
const MAX_FLAG_GROUPS: usize = 12;

/// A maximal run of non-whitespace characters.
struct Token {
    start: usize,
    end: usize,
    /// Whitespace follows (the token is not at the very end of the text).
    space_after: bool,
}

fn tokenize(chars: &[char]) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        if is_space(chars[index]) {
            index += 1;
            continue;
        }
        let start = index;
        while index < chars.len() && !is_space(chars[index]) {
            index += 1;
        }
        tokens.push(Token {
            start,
            end: index,
            space_after: index < chars.len(),
        });
    }
    tokens
}

/// Token indexes reachable from `from` after zero to twelve flag groups.
///
/// A flag group is `-\S+` (at least two characters) optionally followed by a
/// value token that does not start with `-`, and must be followed by
/// whitespace. Both readings (value consumed or not) are explored, which is
/// the nondeterminism the legacy regex resolved by backtracking.
fn flag_run_ends(chars: &[char], tokens: &[Token], from: usize) -> Vec<usize> {
    let mut reached = vec![from];
    let mut frontier = vec![from];
    for _ in 0..MAX_FLAG_GROUPS {
        let mut next: Vec<usize> = Vec::new();
        for &index in &frontier {
            let Some(flag) = tokens.get(index) else {
                continue;
            };
            if chars[flag.start] != '-' || flag.end - flag.start < 2 || !flag.space_after {
                continue;
            }
            next.push(index + 1);
            if let Some(value) = tokens.get(index + 1) {
                if chars[value.start] != '-' && value.space_after {
                    next.push(index + 2);
                }
            }
        }
        next.sort_unstable();
        next.dedup();
        if next.is_empty() {
            break;
        }
        reached.extend(next.iter().copied());
        frontier = next;
    }
    reached.sort_unstable();
    reached.dedup();
    reached
}

fn token_chars<'a>(chars: &'a [char], token: &Token) -> &'a [char] {
    &chars[token.start..token.end]
}

fn token_equals(chars: &[char], token: &Token, literal: &str) -> bool {
    token_chars(chars, token)
        .iter()
        .copied()
        .eq(literal.chars())
}

/// True when the token begins with `[batch-]get-secret-value` followed by a
/// word boundary.
fn is_get_secret_value_token(chars: &[char], token: &Token) -> bool {
    let text = token_chars(chars, token);
    ["get-secret-value", "batch-get-secret-value"]
        .iter()
        .any(|literal| {
            let length = literal.len();
            starts_at(text, 0, literal) && !at(text, length).is_some_and(is_word)
        })
}

/// `aws [flags] secretsmanager [flags] [batch-]get-secret-value\b`, ignoring
/// case; the legacy `CLI_GSV_PATTERN`.
pub(crate) fn matches_cli_get_secret_value(text: &str) -> bool {
    let chars = folded(text);
    let tokens = tokenize(&chars);
    for (index, token) in tokens.iter().enumerate() {
        let text_of_token = token_chars(&chars, token);
        if !token.space_after || !text_of_token.ends_with(&['a', 'w', 's']) {
            continue;
        }
        for service_index in flag_run_ends(&chars, &tokens, index + 1) {
            let Some(service) = tokens.get(service_index) else {
                continue;
            };
            if !service.space_after || !token_equals(&chars, service, "secretsmanager") {
                continue;
            }
            for operation_index in flag_run_ends(&chars, &tokens, service_index + 1) {
                if tokens
                    .get(operation_index)
                    .is_some_and(|operation| is_get_secret_value_token(&chars, operation))
                {
                    return true;
                }
            }
        }
    }
    false
}

// ---------------------------------------------------------------------------
// SDK call shape
// ---------------------------------------------------------------------------

fn paren_follows(chars: &[char], index: usize) -> bool {
    at(chars, skip_spaces(chars, index)) == Some('(')
}

fn optional_separator(chars: &[char], index: usize) -> usize {
    if matches!(at(chars, index), Some('-' | '_')) {
        index + 1
    } else {
        index
    }
}

/// `GetSecretValue(Request|Command)?\s*\(` starting at `index`.
fn plain_sdk_call_at(chars: &[char], index: usize) -> bool {
    const NAME: &str = "getsecretvalue";
    if !starts_at(chars, index, NAME) {
        return false;
    }
    let end = index + NAME.len();
    paren_follows(chars, end)
        || (starts_at(chars, end, "request") && paren_follows(chars, end + "request".len()))
        || (starts_at(chars, end, "command") && paren_follows(chars, end + "command".len()))
}

/// `\.\s*(batch[-_]?)?get[-_]?secret[-_]?value\s*\(` starting at `index`.
fn dotted_sdk_call_at(chars: &[char], index: usize) -> bool {
    if at(chars, index) != Some('.') {
        return false;
    }
    let mut position = skip_spaces(chars, index + 1);
    if starts_at(chars, position, "batch") {
        position = optional_separator(chars, position + "batch".len());
    }
    for (word, separated) in [("get", true), ("secret", true), ("value", false)] {
        if !starts_at(chars, position, word) {
            return false;
        }
        position += word.len();
        if separated {
            position = optional_separator(chars, position);
        }
    }
    paren_follows(chars, position)
}

/// Secrets Manager SDK call invocation shapes (boto3, JS v3 and generic SDK
/// styles), ignoring case; the legacy `SDK_CALL_PATTERN`.
pub(crate) fn matches_sdk_call(text: &str) -> bool {
    let chars = folded(text);
    (0..chars.len())
        .any(|index| plain_sdk_call_at(&chars, index) || dotted_sdk_call_at(&chars, index))
}

// ---------------------------------------------------------------------------
// Secrets Manager agent URL
// ---------------------------------------------------------------------------

const SMA_HOSTS: [&str; 5] = ["localhost", "127.0.0.1", "0.0.0.0", "[::1]", "::1"];
const SMA_SUFFIX: &str = ":2773/secretsmanager/get";

/// A literal Secrets Manager agent URL, case sensitive; the legacy
/// `SMA_PATTERN`.
pub(crate) fn matches_agent_url(text: &str) -> bool {
    SMA_HOSTS
        .iter()
        .any(|host| text.contains(&format!("{host}{SMA_SUFFIX}")))
}

// ---------------------------------------------------------------------------
// Shell structure
// ---------------------------------------------------------------------------

/// Commands that only read text and are exempt from the SDK-mention checks
/// when they are simple (no compound operator).
const READ_ONLY_COMMANDS: [&str; 28] = [
    "grep", "egrep", "fgrep", "rg", "ag", "ack", "cat", "less", "more", "head", "tail", "bat",
    "git", "gh", "find", "ls", "wc", "diff", "awk", "sed", "sort", "uniq", "cut", "tr", "echo",
    "printf", "man", "help",
];

const WRAPPER_COMMANDS: [&str; 7] = ["sudo", "env", "time", "nice", "nohup", "command", "builtin"];

/// `NAME=value` followed by whitespace at the start of `text`; returns the
/// rest after the whitespace.
fn skip_env_assignment(text: &str) -> Option<&str> {
    let mut characters = text.char_indices();
    let (_, first) = characters.next()?;
    if !(first.is_ascii_alphabetic() || first == '_') {
        return None;
    }
    let mut equals = None;
    for (index, c) in characters {
        if c == '=' {
            equals = Some(index);
            break;
        }
        if !(c.is_ascii_alphanumeric() || c == '_') {
            return None;
        }
    }
    let after = &text[equals? + 1..];
    let value_end = after.find(is_space)?;
    Some(after[value_end..].trim_start_matches(is_space))
}

/// A wrapper word followed by whitespace at the start of `text`.
fn skip_wrapper(text: &str) -> Option<&str> {
    for wrapper in WRAPPER_COMMANDS {
        if let Some(rest) = text.strip_prefix(wrapper) {
            if rest.chars().next().is_some_and(is_space) {
                return Some(rest.trim_start_matches(is_space));
            }
        }
    }
    None
}

/// The command's leading token (after assignments and wrappers, without a
/// path prefix) is a known read-only text tool; legacy `_is_read_only_command`.
pub(crate) fn is_read_only_command(command: &str) -> bool {
    let mut rest = command.trim_start_matches(is_space);
    loop {
        if let Some(after) = skip_env_assignment(rest) {
            rest = after;
        } else if let Some(after) = skip_wrapper(rest) {
            rest = after;
        } else {
            break;
        }
    }
    let first = rest
        .split(is_space)
        .find(|part| !part.is_empty())
        .unwrap_or("");
    let program = first.rsplit('/').next().unwrap_or("");
    READ_ONLY_COMMANDS.contains(&program)
}

/// The command may chain more than one command. Legacy: semicolon, ampersand,
/// pipe, backtick and `$(`.
/// Stricter: newline, carriage return, `<(` and `>(` as well.
pub(crate) fn has_compound_operator(command: &str) -> bool {
    let bytes = command.as_bytes();
    bytes.iter().enumerate().any(|(index, byte)| match byte {
        b';' | b'&' | b'|' | b'`' | b'\n' | b'\r' => true,
        b'$' | b'<' | b'>' => bytes.get(index + 1) == Some(&b'('),
        _ => false,
    })
}

// ---------------------------------------------------------------------------
// Inline interpreter execution
// ---------------------------------------------------------------------------

/// End positions of an interpreter word that starts at `index`.
///
/// `python`, `python2`, `python3` and (stricter than the legacy `python3.N`)
/// `pythonN.M`, plus `node`, `ruby` and `perl`.
fn interpreter_ends(chars: &[char], index: usize) -> Vec<usize> {
    let mut ends = Vec::new();
    if starts_at(chars, index, "python") {
        let base = index + "python".len();
        let mut stems = vec![base];
        if matches!(at(chars, base), Some('2' | '3')) {
            stems.push(base + 1);
        }
        for stem in stems {
            ends.push(stem);
            if at(chars, stem) == Some('.') {
                let mut digits_end = stem + 1;
                while at(chars, digits_end).is_some_and(char::is_numeric) {
                    digits_end += 1;
                }
                if digits_end > stem + 1 {
                    ends.push(digits_end);
                }
            }
        }
    }
    for word in ["node", "ruby", "perl"] {
        if starts_at(chars, index, word) {
            ends.push(index + word.len());
        }
    }
    ends
}

/// Whitespace, then a flag cluster whose last letter is `c` or `e`, then whitespace. The legacy
/// pattern accepted `-[a-zA-Z]*c` and `-e` only; a trailing `e` (`perl -ne`, `node -pe`) is stricter.
fn inline_flag_follows(chars: &[char], index: usize) -> bool {
    let after_space = skip_spaces(chars, index);
    if after_space == index || at(chars, after_space) != Some('-') {
        return false;
    }
    let letters_start = after_space + 1;
    let mut letters_end = letters_start;
    while at(chars, letters_end).is_some_and(|c| c.is_ascii_alphabetic()) {
        letters_end += 1;
    }
    letters_end > letters_start
        && matches!(at(chars, letters_end - 1), Some('c' | 'e'))
        && at(chars, letters_end).is_some_and(is_space)
}

/// An interpreter invoked with an inline-code flag (`-c` or `-e`); legacy
/// `_INTERPRETER_INLINE_RE`. Case sensitive.
pub(crate) fn has_inline_interpreter(command: &str) -> bool {
    let chars: Vec<char> = command.chars().collect();
    (0..chars.len()).any(|index| {
        interpreter_ends(&chars, index)
            .into_iter()
            .any(|end| inline_flag_follows(&chars, end))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn space_matches_python_whitespace() {
        for c in [
            ' ', '\t', '\n', '\u{b}', '\u{c}', '\r', '\u{1c}', '\u{1f}', '\u{85}', '\u{a0}',
            '\u{2003}', '\u{3000}',
        ] {
            assert!(is_space(c), "{c:?}");
        }
        for c in ['a', '-', '\u{200b}', '0'] {
            assert!(!is_space(c), "{c:?}");
        }
    }

    #[test]
    fn fold_equates_python_ignorecase_pairs() {
        assert_eq!(fold('A'), 'a');
        assert_eq!(fold('\u{17f}'), 's');
        assert_eq!(fold('\u{212a}'), 'k');
        assert_eq!(fold('\u{130}'), 'i');
        assert_eq!(fold('-'), '-');
    }

    #[test]
    fn cli_shape_positive_cases() {
        for text in [
            "aws secretsmanager get-secret-value --secret-id x",
            "aws secretsmanager batch-get-secret-value --secret-id-list a b",
            "AWS SecretsManager Get-Secret-Value",
            "aws   secretsmanager\tget-secret-value",
            "aws --region us-east-1 secretsmanager get-secret-value",
            "aws --region us-east-1 --profile p secretsmanager --debug get-secret-value --secret-id x",
            "aws -r secretsmanager get-secret-value",
            "aws secretsmanager --region us-east-1 get-secret-value",
            "aws secretsmanager get-secret-value;ls",
            "echo hi && aws secretsmanager get-secret-value",
            "laws secretsmanager get-secret-value",
            "(aws secretsmanager get-secret-value)",
            "aws secretsmanager get-secret-value-extra",
            "aws secretsmanager get-secret-value\u{e9}",
            "aws\u{a0}secretsmanager get-secret-value",
            "a\nb aws secretsmanager\nget-secret-value",
        ] {
            assert!(matches_cli_get_secret_value(text), "{text:?}");
        }
    }

    #[test]
    fn cli_shape_negative_cases() {
        for text in [
            "",
            "aws",
            "aws ",
            "aws s3 ls",
            "aws secretsmanager list-secrets",
            "aws secretsmanager describe-secret --secret-id x",
            "aws secretsmanager get-secret-values",
            "aws secretsmanager get-secret-valueX",
            "aws secretsmanager get-secret-value_",
            "aws secretsmanagerx get-secret-value",
            "awsx secretsmanager get-secret-value",
            "aws secretsmanager",
            "aws secretsmanager ",
            "secretsmanager get-secret-value",
            "aws get-secret-value secretsmanager",
            "aws secretsmanager get-secret-",
            "aws secretsmanager get secret value",
            "aws foo secretsmanager get-secret-value",
            "aws - secretsmanager get-secret-value",
        ] {
            assert!(!matches_cli_get_secret_value(text), "{text:?}");
        }
    }

    #[test]
    fn cli_flag_groups_are_capped_at_twelve() {
        let twelve = "-a ".repeat(12);
        let thirteen = "-a ".repeat(13);
        assert!(matches_cli_get_secret_value(&format!(
            "aws {twelve}secretsmanager get-secret-value"
        )));
        assert!(!matches_cli_get_secret_value(&format!(
            "aws {thirteen}secretsmanager get-secret-value"
        )));
        assert!(matches_cli_get_secret_value(&format!(
            "aws secretsmanager {twelve}get-secret-value"
        )));
        assert!(!matches_cli_get_secret_value(&format!(
            "aws secretsmanager {thirteen}get-secret-value"
        )));
        let valued = "-a v ".repeat(12);
        let valued_over = "-a v ".repeat(13);
        assert!(matches_cli_get_secret_value(&format!(
            "aws {valued}secretsmanager get-secret-value"
        )));
        assert!(!matches_cli_get_secret_value(&format!(
            "aws {valued_over}secretsmanager get-secret-value"
        )));
    }

    #[test]
    fn cli_value_token_is_tried_both_ways() {
        // The value-like word may itself be the service word.
        assert!(matches_cli_get_secret_value(
            "aws --x secretsmanager secretsmanager get-secret-value"
        ));
        assert!(matches_cli_get_secret_value(
            "aws --profile secretsmanager secretsmanager get-secret-value"
        ));
    }

    #[test]
    fn cli_scan_stays_linear_on_long_flag_runs() {
        let long = format!(
            "aws {}secretsmanager get-secret-valu",
            "-a v ".repeat(200_000)
        );
        assert!(!matches_cli_get_secret_value(&long));
        assert!(!matches_cli_get_secret_value(&"aws ".repeat(200_000)));
    }

    #[test]
    fn sdk_shape_positive_cases() {
        for text in [
            "client.get_secret_value(SecretId='x')",
            "client.getSecretValue(params)",
            "client . get_secret_value (x)",
            "client.\nget_secret_value(x)",
            "client.get-secret-value(x)",
            "client.batch_get_secret_value(x)",
            "client.BatchGetSecretValue(x)",
            "client.batch-get-secret-value(x)",
            "client.GETSECRETVALUE(x)",
            "client.getsecret_value(x)",
            "new GetSecretValueCommand({})",
            "new BatchGetSecretValueCommand ({})",
            "GetSecretValueRequest(x)",
            "GetSecretValue(x)",
            "xGetSecretValue(x)",
            "client.\u{a0}get_secret_value(x)",
        ] {
            assert!(matches_sdk_call(text), "{text:?}");
        }
    }

    #[test]
    fn sdk_shape_negative_cases() {
        for text in [
            "",
            "get_secret_value",
            "get_secret_value without a call",
            "client.get_secret_values(x)",
            "client.list_secrets()",
            "client.describe_secret(x)",
            "GetSecretValueCommandX(x)",
            "GetSecretValueResponse(x)",
            "GetSecret(x)",
            "client.get_secret(x)",
            "client.get_secretvalue_x(x)",
            "client.batch_get_secret_value",
            ".get_secret_value",
        ] {
            assert!(!matches_sdk_call(text), "{text:?}");
        }
    }

    #[test]
    fn agent_url_hosts() {
        for host in ["localhost", "127.0.0.1", "0.0.0.0", "[::1]", "::1"] {
            assert!(
                matches_agent_url(&format!(
                    "curl http://{host}:2773/secretsmanager/get?secretId=x"
                )),
                "{host}"
            );
        }
        for text in [
            "curl http://localhost:2774/secretsmanager/get",
            "curl http://localhost:2773/secretsmanager/list",
            "curl http://example.com:2773/secretsmanager/get",
            "curl http://LOCALHOST:2773/secretsmanager/get",
            "curl http://localhost:2773/SecretsManager/get",
            "",
        ] {
            assert!(!matches_agent_url(text), "{text:?}");
        }
    }

    #[test]
    fn read_only_prefixes() {
        for command in [
            "grep -r foo .",
            "  grep foo",
            "/usr/bin/grep foo",
            "FOO=bar grep foo",
            "A=1 B=2 grep foo",
            "sudo grep foo",
            "sudo env FOO=bar time nice nohup command builtin cat x",
            "env cat x",
            "cat",
            "git log",
            "echo hi",
            "printf hi",
            "man aws",
            "help",
        ] {
            assert!(is_read_only_command(command), "{command:?}");
        }
        for command in [
            "",
            "   ",
            "python3 x.py",
            "aws s3 ls",
            "FOO=bar",
            "FOO=bar python3 x",
            "sudo -u root grep x",
            "grepx foo",
            "/usr/bin/grep/ foo",
            "timeout 5 grep x",
            "1FOO=bar grep x",
            "FOO-BAR=x grep x",
            "curl http://x",
            "bash -c grep",
        ] {
            assert!(!is_read_only_command(command), "{command:?}");
        }
    }

    #[test]
    fn compound_operators() {
        for command in [
            "a; b", "a && b", "a & b", "a | b", "a `b`", "a $(b)", "a\nb", "a\r\nb", "a <(b)",
            "a >(b)",
        ] {
            assert!(has_compound_operator(command), "{command:?}");
        }
        for command in [
            "",
            "a b c",
            "a $b",
            "a < b",
            "a > b",
            "a (b)",
            "price $5 (x)",
            "a $",
        ] {
            assert!(!has_compound_operator(command), "{command:?}");
        }
    }

    #[test]
    fn inline_interpreters() {
        for command in [
            "python -c 'x'",
            "python3 -c \"x\"",
            "python2 -c x",
            "python3.11 -c x",
            "python2.7 -c x",
            "python3 -Sc x",
            "python3 -ic x",
            "node -e x",
            "node -pe x",
            "ruby -e x",
            "perl -e x",
            "perl -ne x",
            "node   -e\tx",
            "xnode -e x",
            "sudo python3 -c x",
            "python3 -e x",
        ] {
            assert!(has_inline_interpreter(command), "{command:?}");
        }
        for command in [
            "",
            "python3 script.py",
            "python3 -m pip",
            "python3 -c",
            "python3 -cx",
            "python3 -ex",
            "python3-c x",
            "python3 -1c x",
            "python3 c x",
            "node script.js",
            "node -v",
            "nodejs -e x",
            "Python3 -c x",
            "python3.x -c x",
            "python3. -c x",
            "rubyx -e x",
        ] {
            assert!(!has_inline_interpreter(command), "{command:?}");
        }
    }

    #[test]
    fn contains_folded_ignores_case() {
        assert!(contains_folded("SecretsManager", "secretsmanager"));
        assert!(contains_folded("xSECRETSMANAGERx", "secretsmanager"));
        assert!(!contains_folded("secrets manager", "secretsmanager"));
        assert!(!contains_folded("", "secretsmanager"));
        assert!(!contains_folded("abc", ""));
    }
}

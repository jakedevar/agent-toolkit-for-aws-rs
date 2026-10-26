//! Minimal YAML frontmatter reader for `SKILL.md` files.
//!
//! This is deliberately not a YAML engine: it reads only the two metadata keys
//! this bootstrap slice needs (`name` and `description`) from a leading
//! `---`-delimited block, using plain string handling. Anything it cannot
//! confidently read is left to the caller's fallback (directory name, empty
//! description), which the CLI documents.

/// The subset of `SKILL.md` frontmatter this crate reads.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Frontmatter {
    /// `name:` value, when present as an inline, quoted or block scalar.
    pub name: Option<String>,
    /// `description:` value, when present as an inline, quoted or block scalar.
    pub description: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Key {
    Name,
    Description,
}

impl Key {
    fn parse(raw: &str) -> Option<Self> {
        match raw {
            "name" => Some(Self::Name),
            "description" => Some(Self::Description),
            _ => None,
        }
    }

    fn set(self, result: &mut Frontmatter, value: String) {
        match self {
            Self::Name => result.name = Some(value),
            Self::Description => result.description = Some(value),
        }
    }
}

/// Parses the frontmatter keys `name` and `description` from a Markdown file's
/// leading `---` block. Malformed or missing blocks yield
/// `Frontmatter::default()` rather than an error: discovery must not fail on a
/// skill that merely lacks metadata. The scan is bounded so a file without a
/// closing marker cannot make discovery treat an unbounded body as metadata.
pub fn parse_frontmatter(content: &str) -> Frontmatter {
    let mut result = Frontmatter::default();
    let mut lines = content.lines();
    if lines.next().map(str::trim) != Some("---") {
        return result;
    }

    // Open block scalar: (key, folded style '>', literal style '|') and the
    // body segments collected so far.
    let mut block: Option<(Key, char, Vec<String>)> = None;

    for line in lines.take(256) {
        if line.trim() == "---" || line.trim() == "..." {
            break;
        }

        let is_block_body = |line: &str, block: &Option<(Key, char, Vec<String>)>| {
            block.is_some()
                && (line.starts_with(' ')
                    || line.starts_with('\t')
                    || line.trim().is_empty())
        };
        if is_block_body(line, &block) {
            if let Some((_, _, segments)) = &mut block {
                let text = line.trim();
                if !text.is_empty() {
                    segments.push(text.to_string());
                }
            }
            continue;
        }

        flush_block(&mut block, &mut result);
        let Some((raw_key, value)) = split_key(line) else {
            continue;
        };
        let Some(key) = Key::parse(raw_key) else {
            continue;
        };

        let value = value.trim();
        if let Some(style) = block_scalar_style(value) {
            block = Some((key, style, Vec::new()));
        } else {
            key.set(&mut result, unquote(value));
        }
    }
    flush_block(&mut block, &mut result);

    result
}

/// Joins buffered block-scalar segments into the key's value: folded (`>`)
/// scalars join with a space, literal (`|`) scalars with newlines.
fn flush_block(block: &mut Option<(Key, char, Vec<String>)>, result: &mut Frontmatter) {
    if let Some((key, style, segments)) = block.take() {
        if segments.is_empty() {
            return;
        }
        let separator = if style == '|' { "\n" } else { " " };
        key.set(result, segments.join(separator));
    }
}

/// Splits `key: value` at the first `:` boundary; returns `None` for lines
/// without a top-level key.
fn split_key(line: &str) -> Option<(&str, &str)> {
    let idx = line.find(':')?;
    let key = line[..idx].trim();
    if key.is_empty() || key.contains(' ') || key.contains('#') {
        return None;
    }
    Some((key, line[idx + 1..].trim_start()))
}

fn block_scalar_style(value: &str) -> Option<char> {
    let trimmed = value.trim_end_matches(['+', '-']);
    match trimmed {
        ">" => Some('>'),
        "|" => Some('|'),
        _ => None,
    }
}

/// Strips one layer of matching single or double quotes.
fn unquote(value: &str) -> String {
    let bytes = value.as_bytes();
    if bytes.len() >= 2
        && ((bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\''))
    {
        return value[1..value.len() - 1].to_string();
    }
    value.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_name_and_description() {
        let fm =
            parse_frontmatter("---\nname: aws-auth\ndescription: Adds Cognito auth\n---\n# body\n");
        assert_eq!(fm.name.as_deref(), Some("aws-auth"));
        assert_eq!(fm.description.as_deref(), Some("Adds Cognito auth"));
    }

    #[test]
    fn folded_block_description_joins_with_spaces() {
        let content = "---\nname: s\ndescription: >\n  first part\n  second part\n---\n";
        let fm = parse_frontmatter(content);
        assert_eq!(fm.description.as_deref(), Some("first part second part"));
    }

    #[test]
    fn literal_block_description_joins_with_newlines() {
        let content = "---\ndescription: |\n  line one\n  line two\n---\n";
        let fm = parse_frontmatter(content);
        assert_eq!(fm.description.as_deref(), Some("line one\nline two"));
    }

    #[test]
    fn chomping_indicators_do_not_leak_into_values() {
        let content = "---\ndescription: >-\n  folded chomped\n---\n";
        let fm = parse_frontmatter(content);
        assert_eq!(fm.description.as_deref(), Some("folded chomped"));
    }

    #[test]
    fn quoted_values_are_unquoted() {
        let fm = parse_frontmatter("---\nname: \"quoted name\"\n---\n");
        assert_eq!(fm.name.as_deref(), Some("quoted name"));
    }

    #[test]
    fn missing_frontmatter_marker_yields_default() {
        let content = "no frontmatter here\n---\nname: ignored\n";
        assert_eq!(parse_frontmatter(content), Frontmatter::default());
    }

    #[test]
    fn non_target_keys_are_ignored() {
        let content = "---\nlicense: Apache-2.0\nname: kept\nallowed-tools: Bash\n---\n";
        let fm = parse_frontmatter(content);
        assert_eq!(fm.name.as_deref(), Some("kept"));
        assert_eq!(fm.description, None);
    }

    #[test]
    fn missing_closing_marker_is_bounded() {
        let mut content = String::from("---\nname: ok\n");
        content.push_str(&"filler: x\n".repeat(1000));
        let fm = parse_frontmatter(&content);
        assert_eq!(fm.name.as_deref(), Some("ok"));
    }

    #[test]
    fn blank_block_lines_are_skipped_without_double_separators() {
        let content = "---\ndescription: >\n  one\n\n  two\n---\n";
        let fm = parse_frontmatter(content);
        assert_eq!(fm.description.as_deref(), Some("one two"));
    }
}

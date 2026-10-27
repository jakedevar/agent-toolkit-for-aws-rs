//! Workspace contract for slices that replay recorded AWS calls (#4727).
//!
//! Every slice without a live AWS client used to hand-write its recorded
//! client and its response accessors, and reviews kept finding the same
//! defects: a wrong-typed or `null` recorded member defaulted (#4712, #4724),
//! a `null` response list read as an empty discovery (#4713, #4725), a failure
//! step whose expected request was never compared (#4726). The rules now live
//! once in `toolkit_aws::recorded`; this test keeps the slices on them.
//!
//! The test never keys on a spelling an author chooses (a struct name, a
//! helper name, a nesting of `match`). It works on the token stream of every
//! non-test source file of `crates/toolkit-cli/src`, with comments and string
//! contents removed, and decides in two steps.
//!
//! **Scope** (#4752). A *recorded slice* is a top-level module of `src` (the
//! directory and its `<name>.rs`) with any of these signals:
//!
//! - a recording flag: a string literal `"--recorded"` (also `"--recording"`,
//!   `"--replay"`) in the slice, or a `COMMANDS` row of `lib.rs` that names the
//!   flag, or a constant that holds it, and routes to the slice;
//! - a recorded replay type or trait: any identifier that mentions `Recorded`
//!   or `Replay` together with `Client`, `Step`, `Queue` or `Replay`
//!   (`RecordedClient`, `RecordedAwsClient`, `ReplayClient`, ...);
//! - an import of `toolkit_aws::recorded` in any path form: plain, grouped
//!   (`toolkit_aws::{recorded, ..}`), aliased (`recorded as r`,
//!   `toolkit_aws as aws` then `aws::recorded`), in a nested module.
//!
//! A slice whose only signal is the import adopted the module already. A slice
//! with a flag or a type signal and no import fails the adoption check.
//!
//! **Checks** on every file of a recorded slice (#4751):
//!
//! 1. it defines none of the shared helpers (`optional_member`,
//!    `optional_list`, `optional_object`, `optional_text`, `same_json`, ...);
//! 2. it reads no JSON shape: a `Json::Array`, `Json::Object`, `Value::Array`
//!    or `Value::Object` in a pattern (`match`, `if let`, `let ... else`,
//!    `matches!` in any macro delimiter (#4794), a guard, behind `Some(..)`, a
//!    struct-field pattern, a `let` with a type annotation `let P: T = ..`
//!    (#4793), tuple form `Json::Array(v)` and brace form
//!    `Json::Array { 0: v }` or
//!    `Json::Object { .. }` alike (#4774); also through an alias or a
//!    `use Json::{Array, Object}` import), and any call or path `as_array`,
//!    `as_object`, `as_array_mut`, `as_object_mut`, is refused unless a marker
//!    `// recorded-shape: <reason>` allows it. Building a value is not a read;
//! 3. it does not alias the JSON value type (#4775): `type J = Json;`,
//!    `type V = serde_json::Value;`, `use ..::Json as J`, a re-export, an
//!    alias of an alias. A marked alias is allowed but its reads still count,
//!    and an alias defined anywhere in `src` is followed into the slice;
//! 4. every marker has a specific reason (#4773): at least three distinct
//!    words, placeholders (`todo`, `x`, `<reason>`) not counted. A refused
//!    marker allows nothing. A marker allows its own line; above code (the
//!    comment block in between continues the reason) it allows the next code
//!    line, or the whole of a function, method or closure-binding `let` that
//!    starts there. Never a `mod`, an `impl` or `trait` block, another item or
//!    an inner-attribute (file header) scope. The reason says why the value is
//!    not a response member (a snapshot file, the command's own manifest, a
//!    value the slice built) or why the member is required and every other
//!    shape is rejected.
//!
//! This is a tripwire for the documented rule, not a proof: it has no type
//! inference and a determined author can still hide a read. Review remains
//! the defense against deliberate evasion, and evading the scanner is a review
//! blocker.
//!
//! Test code (`tests.rs`, `tests/`, `#[cfg(test)]` items) is out of scope. The
//! README section "Slices without a live AWS client" is the guide for slice
//! authors.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};

/// The crate and module every recorded slice reads through.
const SHARED_CRATE: &str = "toolkit_aws";
const SHARED_MODULE: &str = "recorded";
/// Flags that declare a recording input.
const RECORDING_FLAGS: [&str; 3] = ["--recorded", "--recording", "--replay"];
/// The allow-list marker; the text after it is the reason.
const MARKER: &str = "recorded-shape:";
/// JSON enum types whose variants expose an array or an object.
const SHAPE_TYPES: [&str; 2] = ["Json", "Value"];
/// The variants that expose a shape.
const SHAPE_VARIANTS: [&str; 2] = ["Array", "Object"];
/// Methods that expose a shape.
const SHAPE_METHODS: [&str; 4] = ["as_array", "as_object", "as_array_mut", "as_object_mut"];
/// Helpers a recorded slice must take from the shared module, never redefine.
/// An extra signal only: renamed copies are caught by the shape read ban.
const SHARED_HELPERS: [&str; 8] = [
    "optional_member",
    "optional_list",
    "optional_object",
    "optional_object_list",
    "optional_string_list",
    "optional_text",
    "nullable_text",
    "same_json",
];
/// Words of an identifier that make it a recorded replay type or trait.
const REPLAY_WORDS: [&str; 2] = ["Recorded", "Replay"];
const REPLAY_PARTS: [&str; 4] = ["Client", "Step", "Queue", "Replay"];
/// A marker reason needs at least this many distinct words (#4773).
const MIN_REASON_WORDS: usize = 3;
/// Words that never count towards a reason: a marker whose reason is only
/// these (`todo`, `<reason>`, `fix later`) is refused.
const PLACEHOLDER_WORDS: [&str; 16] = [
    "todo",
    "tbd",
    "fixme",
    "xxx",
    "reason",
    "placeholder",
    "wip",
    "later",
    "none",
    "na",
    "ok",
    "hack",
    "temp",
    "ignore",
    "skip",
    "allow",
];

struct SourceFile {
    /// Path relative to `src`, with `/` separators.
    relative: String,
    text: String,
}

fn collect(dir: &Path, root: &Path, out: &mut Vec<SourceFile>) {
    let mut entries: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap_or_else(|error| panic!("read {}: {error}", dir.display()))
        .map(|entry| entry.expect("directory entry").path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            collect(&path, root, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            let relative = path
                .strip_prefix(root)
                .expect("under src")
                .components()
                .map(|part| part.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            let text = fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
            out.push(SourceFile { relative, text });
        }
    }
}

/// Every non-test source file of `crates/toolkit-cli/src`.
fn sources() -> Vec<SourceFile> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    collect(&root, &root, &mut files);
    files.retain(|file| !is_test_file(&file.relative));
    files
}

fn is_test_file(relative: &str) -> bool {
    let parts: Vec<&str> = relative.split('/').collect();
    parts.last() == Some(&"tests.rs") || parts[..parts.len() - 1].contains(&"tests")
}

/// The top-level module a file belongs to: `ai_models/hub.rs` and
/// `ai_models.rs` are both `ai_models`.
fn slice_of(relative: &str) -> &str {
    let first = relative.split('/').next().unwrap_or(relative);
    first.strip_suffix(".rs").unwrap_or(first)
}

// ---------------------------------------------------------------- lexing

#[derive(Default)]
struct Lexed {
    /// The source with comments and string/char contents blanked to spaces;
    /// newlines are kept, so line numbers are those of the source.
    code: String,
    /// The comment text of each line.
    comments: Vec<String>,
    /// The contents of the string literals, in source order.
    strings: VecDeque<String>,
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Splits `source` into code, comments and string contents. It understands
/// line comments, nested block comments, strings with escapes, raw strings,
/// character literals and lifetimes.
fn lex(source: &str) -> Lexed {
    let chars: Vec<char> = source.chars().collect();
    let mut out = Lexed {
        comments: vec![String::new()],
        ..Lexed::default()
    };
    let newline = |out: &mut Lexed| {
        out.code.push('\n');
        out.comments.push(String::new());
    };
    let mut i = 0;
    let mut block_depth = 0usize;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        if block_depth > 0 {
            if c == '/' && next == Some('*') {
                block_depth += 1;
                i += 2;
            } else if c == '*' && next == Some('/') {
                block_depth -= 1;
                i += 2;
            } else {
                if c == '\n' {
                    newline(&mut out);
                } else {
                    out.code.push(' ');
                    out.comments.last_mut().expect("a line").push(c);
                }
                i += 1;
            }
            continue;
        }
        let prev_is_ident = i > 0 && is_ident_char(chars[i - 1]);
        match c {
            '\n' => {
                newline(&mut out);
                i += 1;
            }
            '/' if next == Some('/') => {
                while i < chars.len() && chars[i] != '\n' {
                    out.code.push(' ');
                    out.comments.last_mut().expect("a line").push(chars[i]);
                    i += 1;
                }
            }
            '/' if next == Some('*') => {
                block_depth = 1;
                out.code.push_str("  ");
                i += 2;
            }
            '"' => {
                i = lex_string(&chars, i, &mut out, &newline);
            }
            'r' | 'b' if !prev_is_ident && raw_string_open(&chars, i).is_some() => {
                let (open, hashes) = raw_string_open(&chars, i).expect("checked");
                i = lex_raw_string(&chars, open, hashes, &mut out, &newline);
            }
            '\'' => {
                let end = if next == Some('\\') {
                    (i + 3..chars.len()).find(|&j| chars[j] == '\'')
                } else if chars.get(i + 2) == Some(&'\'') {
                    Some(i + 2)
                } else {
                    None
                };
                if let Some(end) = end {
                    out.code.push('\'');
                    for _ in i + 1..end {
                        out.code.push(' ');
                    }
                    out.code.push('\'');
                    i = end + 1;
                } else {
                    out.code.push('\'');
                    i += 1;
                }
            }
            _ => {
                out.code.push(c);
                i += 1;
            }
        }
    }
    out
}

/// For `r"`, `r#"`, `br"`, `br#"` at `start`: the index of the opening quote
/// and the number of hashes.
fn raw_string_open(chars: &[char], start: usize) -> Option<(usize, usize)> {
    let mut j = start;
    if chars.get(j) == Some(&'b') {
        j += 1;
    }
    if chars.get(j) != Some(&'r') {
        return None;
    }
    j += 1;
    let mut hashes = 0;
    while chars.get(j) == Some(&'#') {
        hashes += 1;
        j += 1;
    }
    (chars.get(j) == Some(&'"')).then_some((j, hashes))
}

fn lex_string(chars: &[char], open: usize, out: &mut Lexed, newline: &dyn Fn(&mut Lexed)) -> usize {
    out.code.push('"');
    let mut content = String::new();
    let mut i = open + 1;
    while i < chars.len() && chars[i] != '"' {
        let c = chars[i];
        if c == '\\' && i + 1 < chars.len() {
            content.push(chars[i + 1]);
            out.code.push(' ');
            if chars[i + 1] == '\n' {
                newline(out);
            } else {
                out.code.push(' ');
            }
            i += 2;
        } else {
            content.push(c);
            if c == '\n' {
                newline(out);
            } else {
                out.code.push(' ');
            }
            i += 1;
        }
    }
    out.code.push('"');
    out.strings.push_back(content);
    i + 1
}

fn lex_raw_string(
    chars: &[char],
    open: usize,
    hashes: usize,
    out: &mut Lexed,
    newline: &dyn Fn(&mut Lexed),
) -> usize {
    out.code.push('"');
    let mut content = String::new();
    let mut i = open + 1;
    while i < chars.len() {
        let closes = chars[i] == '"' && (1..=hashes).all(|k| chars.get(i + k) == Some(&'#'));
        if closes {
            break;
        }
        content.push(chars[i]);
        if chars[i] == '\n' {
            newline(out);
        } else {
            out.code.push(' ');
        }
        i += 1;
    }
    out.code.push('"');
    out.strings.push_back(content);
    i + 1 + hashes
}

#[derive(Clone)]
struct Tok {
    text: String,
    /// 0-based line.
    line: usize,
    /// A string literal; `text` is its content.
    is_str: bool,
}

impl Tok {
    fn is(&self, text: &str) -> bool {
        !self.is_str && self.text == text
    }
    fn is_ident(&self) -> bool {
        !self.is_str
            && self
                .text
                .chars()
                .next()
                .is_some_and(|c| c.is_alphabetic() || c == '_')
    }
}

fn tokenize(lexed: &mut Lexed) -> Vec<Tok> {
    let chars: Vec<char> = lexed.code.chars().collect();
    let mut toks = Vec::new();
    let mut line = 0;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\n' {
            line += 1;
            i += 1;
        } else if c.is_whitespace() {
            i += 1;
        } else if is_ident_char(c) {
            let start = i;
            while i < chars.len() && is_ident_char(chars[i]) {
                i += 1;
            }
            toks.push(Tok {
                text: chars[start..i].iter().collect(),
                line,
                is_str: false,
            });
        } else if c == '"' {
            let start_line = line;
            i += 1;
            while i < chars.len() && chars[i] != '"' {
                if chars[i] == '\n' {
                    line += 1;
                }
                i += 1;
            }
            i += 1;
            toks.push(Tok {
                text: lexed.strings.pop_front().unwrap_or_default(),
                line: start_line,
                is_str: true,
            });
        } else if c == ':' && chars.get(i + 1) == Some(&':') {
            toks.push(Tok {
                text: "::".into(),
                line,
                is_str: false,
            });
            i += 2;
        } else {
            toks.push(Tok {
                text: c.to_string(),
                line,
                is_str: false,
            });
            i += 1;
        }
    }
    toks
}

/// The index of the token closing the bracket opened at `open`.
fn matching(toks: &[Tok], open: usize) -> usize {
    let mut depth = 0i32;
    for (index, tok) in toks.iter().enumerate().skip(open) {
        if tok.is("(") || tok.is("[") || tok.is("{") {
            depth += 1;
        } else if tok.is(")") || tok.is("]") || tok.is("}") {
            depth -= 1;
            if depth == 0 {
                return index;
            }
        }
    }
    toks.len().saturating_sub(1)
}

/// The index of the last token of the item or statement starting at `start`:
/// its first top-level `;`, or the `}` closing its first top-level `{`.
fn item_end(toks: &[Tok], start: usize) -> usize {
    let mut depth = 0i32;
    let mut i = start;
    while i < toks.len() {
        let tok = &toks[i];
        if tok.is("{") && depth == 0 {
            return matching(toks, i);
        }
        if tok.is("(") || tok.is("[") || tok.is("{") {
            depth += 1;
        } else if tok.is(")") || tok.is("]") || tok.is("}") {
            depth -= 1;
        } else if tok.is(";") && depth == 0 {
            return i;
        }
        i += 1;
    }
    toks.len().saturating_sub(1)
}

/// Whether the tokens from `start` begin a leaf item a marker may cover whole
/// (#4773): a function or method (after outer attributes, visibility and
/// qualifiers), or a `let` statement that binds a closure. A `mod`, an `impl`
/// or `trait` block, any other item and an inner attribute (`#![..]`, a file
/// or module header) are not leaves: a marker there covers its line only.
fn starts_leaf_item(toks: &[Tok], start: usize) -> bool {
    let mut i = start;
    loop {
        match toks.get(i) {
            Some(tok) if tok.is("#") => {
                if !toks.get(i + 1).is_some_and(|t| t.is("[")) {
                    // `#![..]` is an inner attribute: it scopes a whole module.
                    return false;
                }
                i = matching(toks, i + 1) + 1;
            }
            Some(tok) if tok.is("pub") => {
                i += 1;
                if toks.get(i).is_some_and(|t| t.is("(")) {
                    i = matching(toks, i) + 1;
                }
            }
            Some(tok) if tok.is("const") || tok.is("unsafe") || tok.is("async") => i += 1,
            Some(tok) if tok.is("extern") => {
                i += 1;
                if toks.get(i).is_some_and(|t| t.is_str) {
                    i += 1;
                }
            }
            Some(tok) if tok.is("fn") => return true,
            Some(tok) if tok.is("let") => return binds_closure(toks, i),
            _ => return false,
        }
    }
}

/// Whether the `let` at `start` binds a closure (`let f = |v| ..;`,
/// `let f = move || ..;`).
fn binds_closure(toks: &[Tok], start: usize) -> bool {
    let mut depth = 0i32;
    for (i, tok) in toks.iter().enumerate().skip(start + 1) {
        if tok.is("(") || tok.is("[") || tok.is("{") {
            depth += 1;
        } else if tok.is(")") || tok.is("]") || tok.is("}") {
            depth -= 1;
        } else if tok.is(";") && depth <= 0 {
            return false;
        } else if tok.is("=") && depth == 0 {
            let next = toks.get(i + 1);
            if next.is_some_and(|t| t.is("=") || t.is(">")) {
                return false;
            }
            let mut value = i + 1;
            if toks.get(value).is_some_and(|t| t.is("move")) {
                value += 1;
            }
            return toks.get(value).is_some_and(|t| t.is("|"));
        }
    }
    false
}

/// Removes `#[cfg(test)]` items, which are not production code.
fn drop_test_items(toks: Vec<Tok>) -> Vec<Tok> {
    let mut drop = vec![false; toks.len()];
    let mut i = 0;
    while i + 6 < toks.len() {
        let is_cfg_test = toks[i].is("#")
            && toks[i + 1].is("[")
            && toks[i + 2].is("cfg")
            && toks[i + 3].is("(")
            && toks[i + 4].is("test")
            && toks[i + 5].is(")")
            && toks[i + 6].is("]");
        if is_cfg_test {
            let end = item_end(&toks, i + 7);
            for flag in &mut drop[i..=end.min(toks.len() - 1)] {
                *flag = true;
            }
            i = end + 1;
        } else {
            i += 1;
        }
    }
    toks.into_iter()
        .zip(drop)
        .filter_map(|(tok, dropped)| (!dropped).then_some(tok))
        .collect()
}

struct Parsed {
    relative: String,
    slice: String,
    text: String,
    comments: Vec<String>,
    /// Whether each line carries code.
    code_lines: Vec<bool>,
    toks: Vec<Tok>,
}

fn parse(file: &SourceFile) -> Parsed {
    let mut lexed = lex(&file.text);
    let toks = drop_test_items(tokenize(&mut lexed));
    let code_lines = lexed
        .code
        .split('\n')
        .map(|l| !l.trim().is_empty())
        .collect();
    Parsed {
        relative: file.relative.clone(),
        slice: slice_of(&file.relative).to_string(),
        text: file.text.clone(),
        comments: lexed.comments,
        code_lines,
        toks,
    }
}

// --------------------------------------------------------------- the rules

/// Whether a marker reason is specific: at least [`MIN_REASON_WORDS`]
/// distinct words of two or more letters, placeholders not counted (#4773).
fn is_specific_reason(reason: &str) -> bool {
    let words: BTreeSet<String> = reason
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| word.chars().filter(|c| c.is_alphabetic()).count() >= 2)
        .map(str::to_lowercase)
        .filter(|word| !PLACEHOLDER_WORDS.contains(&word.as_str()))
        .collect();
    words.len() >= MIN_REASON_WORDS
}

/// What the markers of a file allow.
#[derive(Default)]
struct Markers {
    /// The 0-based lines they allow.
    allowed: BTreeSet<usize>,
    /// The 0-based lines of markers refused for a blank or placeholder reason;
    /// they allow nothing.
    refused: Vec<usize>,
}

/// The markers of `file`. A marker allows its own line, or, above it, the
/// next code line (the comment block in between is part of the reason). When
/// that line starts a leaf item (a function or method, or a `let` binding a
/// closure; see [`starts_leaf_item`]) the marker above covers the whole item.
/// Never a `mod`, an `impl` or `trait` block or an inner-attribute scope.
fn markers(file: &Parsed) -> Markers {
    let mut markers = Markers::default();
    for (marker_line, comment) in file.comments.iter().enumerate() {
        let Some(at) = comment.find(MARKER) else {
            continue;
        };
        let mut reason = comment[at + MARKER.len()..].to_string();
        let on_code = file.code_lines.get(marker_line) == Some(&true);
        let target = if on_code {
            Some(marker_line)
        } else {
            // The comment block directly above a code line.
            let mut below = marker_line + 1;
            while below < file.comments.len()
                && !file.code_lines[below]
                && !file.comments[below].trim().is_empty()
            {
                if !file.comments[below].contains(MARKER) {
                    reason.push(' ');
                    reason.push_str(&file.comments[below]);
                }
                below += 1;
            }
            (below < file.comments.len() && file.code_lines[below]).then_some(below)
        };
        if !is_specific_reason(&reason) {
            markers.refused.push(marker_line);
            continue;
        }
        let Some(target) = target else { continue };
        markers.allowed.insert(target);
        if on_code {
            continue;
        }
        if let Some(first) = file.toks.iter().position(|tok| tok.line >= target) {
            if file.toks[first].line == target && starts_leaf_item(&file.toks, first) {
                let end = item_end(&file.toks, first);
                markers.allowed.extend(target..=file.toks[end].line);
            }
        }
    }
    markers
}

/// Which tokens sit inside a `use` item.
fn use_tokens(toks: &[Tok]) -> Vec<bool> {
    let mut inside = vec![false; toks.len()];
    let mut i = 0;
    while i < toks.len() {
        let item_start = i == 0
            || toks[i - 1].is(";")
            || toks[i - 1].is("}")
            || toks[i - 1].is("{")
            || toks[i - 1].is("]")
            || toks[i - 1].is(")")
            || toks[i - 1].is("pub");
        if toks[i].is("use") && item_start {
            let end = item_end(toks, i);
            for flag in &mut inside[i..=end] {
                *flag = true;
            }
            i = end + 1;
        } else {
            i += 1;
        }
    }
    inside
}

/// The first token of the path that ends at `last` (`a::b::C` -> `a`).
fn path_start(toks: &[Tok], last: usize) -> usize {
    let mut start = last;
    while start >= 2 && toks[start - 1].is("::") && toks[start - 2].is_ident() {
        start -= 2;
    }
    start
}

/// The unmatched `(`, `[` or `{` around the token at `start`.
fn enclosing_open(toks: &[Tok], start: usize) -> Option<usize> {
    let mut depth = 0i32;
    for index in (0..start).rev() {
        let tok = &toks[index];
        if tok.is(")") || tok.is("]") || tok.is("}") {
            depth += 1;
        } else if tok.is("(") || tok.is("[") || tok.is("{") {
            if depth == 0 {
                return Some(index);
            }
            depth -= 1;
        }
    }
    None
}

/// The start of the call, macro or tuple whose bracket opens at `open`.
fn call_start(toks: &[Tok], open: usize) -> usize {
    let mut start = open;
    if start >= 1 && toks[start - 1].is("!") {
        start -= 1;
    }
    if start >= 1 && toks[start - 1].is_ident() {
        start = path_start(toks, start - 1);
    }
    start
}

/// Whether the `:` at `colon` starts the type annotation of a `let` pattern
/// (`let PATTERN: Type = ..`, #4793): a type, then an `=` that is neither `==`
/// nor `=>`. Anything that ends the type first (a `;`, a block, a closing
/// bracket, a `,` or `|` at the top level) is not a `let` annotation: a
/// parameter's type is no read.
fn annotated_let(toks: &[Tok], colon: usize) -> bool {
    let mut depth = 0i32;
    for (index, tok) in toks.iter().enumerate().skip(colon + 1) {
        let after_arrow = index > 0 && toks[index - 1].is("-");
        if tok.is("(") || tok.is("[") || tok.is("<") {
            depth += 1;
        } else if tok.is(")") || tok.is("]") || (tok.is(">") && !after_arrow) {
            depth -= 1;
            if depth < 0 {
                return false;
            }
        } else if depth == 0 {
            if tok.is("=") {
                let next = toks.get(index + 1);
                return !next.is_some_and(|t| t.is("=") || t.is(">"));
            }
            if tok.is(";") || tok.is("{") || tok.is("}") || tok.is(",") || tok.is("|") {
                return false;
            }
        }
    }
    false
}

/// Whether the expression or pattern covering `start..=end` is in a pattern
/// position: followed by `=>`, `=` (a `let`), `: Type =` (an annotated `let`,
/// #4793), `|` or a guard `if`, directly or through the tuple-struct, tuple,
/// slice, struct-field or `matches!` pattern around it.
fn in_pattern_position(toks: &[Tok], start: usize, end: usize) -> bool {
    let Some(after) = toks.get(end + 1) else {
        return false;
    };
    let next = toks.get(end + 2);
    if after.is("=") {
        return !next.is_some_and(|t| t.is("="));
    }
    if after.is("|") {
        return !next.is_some_and(|t| t.is("|"));
    }
    if after.is("if") {
        return true;
    }
    if after.is(":") {
        // `let PATTERN: Type = ..` reads like `let PATTERN = ..` (#4793).
        return annotated_let(toks, end + 1);
    }
    if !(after.is(",") || after.is(")") || after.is("]") || after.is("}")) {
        return false;
    }
    let Some(open) = enclosing_open(toks, start) else {
        return false;
    };
    if open >= 2 && toks[open - 1].is("!") && toks[open - 2].is("matches") {
        // `matches!(value, PATTERN)`, in any macro delimiter (#4794):
        // everything after the first comma.
        let mut depth = 0i32;
        for tok in &toks[open + 1..start] {
            if tok.is("(") || tok.is("[") || tok.is("{") {
                depth += 1;
            } else if tok.is(")") || tok.is("]") || tok.is("}") {
                depth -= 1;
            } else if tok.is(",") && depth == 0 {
                return true;
            }
        }
        return false;
    }
    if toks[open].is("{") {
        // A field of a struct pattern or of a brace-form variant (#4774):
        // `Holder { body: Json::Array(v), .. }`, `Some { 0: Json::Array(v) }`.
        // Any other `{` opens a block, whose value is not a pattern.
        let is_field = start >= 3
            && toks[start - 1].is(":")
            && (toks[start - 3].is("{") || toks[start - 3].is(","))
            && toks[start - 2].text.chars().all(is_ident_char)
            && !toks[start - 2].is_str
            && open >= 1
            && toks[open - 1].is_ident();
        return is_field && in_pattern_position(toks, call_start(toks, open), matching(toks, open));
    }
    if after.is("}") {
        return false;
    }
    in_pattern_position(toks, call_start(toks, open), matching(toks, open))
}

/// Whether the variant path ending at `variant` is matched on, as opposed to
/// built.
fn is_pattern(toks: &[Tok], variant: usize) -> bool {
    let start = path_start(toks, variant);
    if start > 0 && toks[start - 1].is("@") {
        return true;
    }
    // A tuple-form `Json::Array(v)` and a brace-form `Json::Array { 0: v }`
    // or `Json::Object { .. }` payload alike (#4774).
    let end = if toks
        .get(variant + 1)
        .is_some_and(|t| t.is("(") || t.is("{"))
    {
        matching(toks, variant + 1)
    } else {
        variant
    };
    in_pattern_position(toks, start, end)
}

/// The aliases of the JSON value type that `toks` defines, given the names
/// already known for it: `(0-based line, alias)` for each `T as A` (a `use`,
/// a re-export, a group member) and each `type A<..> = path::T;`. No type
/// inference: only a path that ends in a known name counts.
fn json_aliases(toks: &[Tok], types: &BTreeSet<String>) -> Vec<(usize, String)> {
    let known = |tok: &Tok| tok.is_ident() && types.contains(&tok.text);
    let in_use = use_tokens(toks);
    let mut found = Vec::new();
    for (i, window) in toks.windows(3).enumerate() {
        // In a `use` only: `<Json as Trait>::f` is not an alias.
        if in_use[i]
            && known(&window[0])
            && window[1].is("as")
            && window[2].is_ident()
            && !window[2].is("_")
        {
            found.push((window[0].line, window[2].text.clone()));
        }
    }
    for (i, tok) in toks.iter().enumerate() {
        let Some(name) = toks.get(i + 1).filter(|t| tok.is("type") && t.is_ident()) else {
            continue;
        };
        let mut eq = i + 2;
        if toks.get(eq).is_some_and(|t| t.is("<")) {
            let mut depth = 0i32;
            while let Some(t) = toks.get(eq) {
                if t.is("<") {
                    depth += 1;
                } else if t.is(">") {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                eq += 1;
            }
            eq += 1;
        }
        if !toks.get(eq).is_some_and(|t| t.is("=")) {
            continue;
        }
        // `path::T;` with only identifiers and `::` in between.
        let mut j = eq + 1;
        let mut last = None;
        while let Some(t) = toks.get(j) {
            if t.is(";") {
                break;
            }
            if !(t.is("::") || t.is_ident()) {
                last = None;
                break;
            }
            if t.is_ident() {
                last = Some(t);
            }
            j += 1;
        }
        if last.is_some_and(known) && toks.get(j).is_some_and(|t| t.is(";")) {
            found.push((tok.line, name.text.clone()));
        }
    }
    found
}

/// Every name of the JSON value type in `files`: `Json`, `Value` and their
/// aliases, followed to a fixed point (an alias of an alias, a re-export in
/// another module).
fn json_type_names(files: &[Parsed]) -> BTreeSet<String> {
    let mut types: BTreeSet<String> = SHAPE_TYPES.iter().map(ToString::to_string).collect();
    loop {
        let before = types.len();
        for file in files {
            for (_, alias) in json_aliases(&file.toks, &types.clone()) {
                types.insert(alias);
            }
        }
        if types.len() == before {
            return types;
        }
    }
}

/// The 0-based lines that read a JSON shape, whether allowed or not; `types`
/// are the names of the JSON value type ([`json_type_names`]).
fn shape_reads(toks: &[Tok], types: &BTreeSet<String>) -> BTreeSet<usize> {
    let in_use = use_tokens(toks);
    let is_variant = |tok: &Tok| SHAPE_VARIANTS.iter().any(|variant| tok.is(variant));
    // Names the file brings into scope with `use Json::{Array, Object}`,
    // `use Json::*` or `use Json::Array as List`; they are shape reads too.
    let mut bare: BTreeSet<&str> = BTreeSet::new();
    let mut lines = BTreeSet::new();
    for (i, tok) in toks.iter().enumerate() {
        if !tok.is_ident() {
            continue;
        }
        if SHAPE_METHODS.iter().any(|method| tok.is(method)) {
            lines.insert(tok.line);
        }
        let is_type = types.contains(tok.text.as_str());
        if !is_type || !toks.get(i + 1).is_some_and(|t| t.is("::")) {
            continue;
        }
        let Some(after) = toks.get(i + 2) else {
            continue;
        };
        if is_variant(after) {
            if in_use[i] {
                lines.insert(tok.line);
                if let Some(alias) = toks.get(i + 4).filter(|_| toks[i + 3].is("as")) {
                    bare.insert(&alias.text);
                }
            } else if is_pattern(toks, i + 2) {
                lines.insert(tok.line);
            }
        } else if in_use[i] && after.is("*") {
            lines.insert(tok.line);
            bare.extend(SHAPE_VARIANTS);
        } else if in_use[i] && after.is("{") {
            let group = &toks[i + 2..=matching(toks, i + 2)];
            for (offset, member) in group.iter().enumerate() {
                if is_variant(member) {
                    lines.insert(member.line);
                    bare.insert(&member.text);
                    if let Some(alias) =
                        group.get(offset + 2).filter(|_| group[offset + 1].is("as"))
                    {
                        bare.insert(&alias.text);
                    }
                }
            }
        }
    }
    for (i, tok) in toks.iter().enumerate() {
        let after_path = i > 0 && toks[i - 1].is("::");
        if tok.is_ident()
            && bare.contains(tok.text.as_str())
            && !after_path
            && !in_use[i]
            && is_pattern(toks, i)
        {
            lines.insert(tok.line);
        }
    }
    lines
}

/// What the read rules find in one file of a recorded slice, each entry
/// `file:line: source` (1-based line).
#[derive(Default)]
struct FileFindings {
    /// Shape reads no marker allows.
    reads: Vec<String>,
    /// Aliases of the JSON value type no marker allows (#4775).
    aliases: Vec<String>,
    /// Markers refused for a blank or placeholder reason (#4773).
    markers: Vec<String>,
}

fn file_findings(file: &Parsed, types: &BTreeSet<String>) -> FileFindings {
    let markers = markers(file);
    let source_lines: Vec<&str> = file.text.lines().collect();
    let entry = |line: usize| {
        format!(
            "{}:{}: {}",
            file.relative,
            line + 1,
            source_lines.get(line).unwrap_or(&"").trim()
        )
    };
    let unmarked = |lines: BTreeSet<usize>| -> Vec<String> {
        lines
            .into_iter()
            .filter(|line| !markers.allowed.contains(line))
            .map(entry)
            .collect()
    };
    let alias_lines = json_aliases(&file.toks, types)
        .into_iter()
        .map(|(line, _)| line)
        .collect();
    FileFindings {
        reads: unmarked(shape_reads(&file.toks, types)),
        aliases: unmarked(alias_lines),
        markers: markers.refused.iter().copied().map(entry).collect(),
    }
}

/// The shared helpers `file` defines itself.
fn redefined_helpers(file: &Parsed) -> Vec<&'static str> {
    SHARED_HELPERS
        .into_iter()
        .filter(|helper| {
            file.toks
                .windows(2)
                .any(|pair| pair[0].is("fn") && pair[1].is(helper))
        })
        .collect()
}

/// Whether `file` imports (or paths into) `toolkit_aws::recorded` in any form.
fn imports_shared(file: &Parsed) -> bool {
    let toks = &file.toks;
    let mut crates: BTreeSet<&str> = BTreeSet::from([SHARED_CRATE]);
    // `toolkit_aws as aws`, `extern crate toolkit_aws as aws`.
    for window in toks.windows(3) {
        if window[0].is(SHARED_CRATE) && window[1].is("as") && window[2].is_ident() {
            crates.insert(&window[2].text);
        }
    }
    // `toolkit_aws::{self as aws, ..}`.
    for (i, tok) in toks.iter().enumerate() {
        if tok.is(SHARED_CRATE)
            && toks.get(i + 1).is_some_and(|t| t.is("::"))
            && toks.get(i + 2).is_some_and(|t| t.is("{"))
        {
            let group = &toks[i + 2..=matching(toks, i + 2)];
            for window in group.windows(3) {
                if window[0].is("self") && window[1].is("as") && window[2].is_ident() {
                    crates.insert(&window[2].text);
                }
            }
        }
    }
    toks.iter().enumerate().any(|(i, tok)| {
        if !tok.is_ident() || !crates.contains(tok.text.as_str()) {
            return false;
        }
        if !toks.get(i + 1).is_some_and(|t| t.is("::")) {
            return false;
        }
        match toks.get(i + 2) {
            Some(next) if next.is(SHARED_MODULE) => true,
            Some(next) if next.is("{") => toks[i + 2..=matching(toks, i + 2)]
                .iter()
                .any(|member| member.is(SHARED_MODULE)),
            _ => false,
        }
    })
}

fn is_replay_name(name: &str) -> bool {
    REPLAY_WORDS.iter().any(|word| name.contains(word))
        && REPLAY_PARTS.iter().any(|part| name.contains(part))
}

/// Whether `file` mentions a recorded replay type or trait.
fn mentions_replay_type(file: &Parsed) -> bool {
    file.toks
        .iter()
        .any(|tok| tok.is_ident() && is_replay_name(&tok.text))
}

fn is_recording_flag(tok: &Tok) -> bool {
    tok.is_str && RECORDING_FLAGS.contains(&tok.text.as_str())
}

/// Whether `file` declares a recording flag as a string literal.
fn declares_recording_flag(file: &Parsed) -> bool {
    file.toks.iter().any(is_recording_flag)
}

/// The names of the constants (anywhere) whose definition holds a recording flag.
fn flag_constants(files: &[Parsed]) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for file in files {
        for (i, tok) in file.toks.iter().enumerate() {
            let is_const = tok.is("const") || tok.is("static");
            let Some(name) = file.toks.get(i + 1).filter(|t| is_const && t.is_ident()) else {
                continue;
            };
            let end = item_end(&file.toks, i);
            if file.toks[i..=end].iter().any(is_recording_flag) {
                names.insert(name.text.clone());
            }
        }
    }
    names
}

/// The slices a `COMMANDS` row of `lib.rs` routes to while naming a recording
/// flag, inline or through a constant.
fn command_slices(files: &[Parsed], constants: &BTreeSet<String>) -> BTreeSet<String> {
    let known: BTreeSet<&str> = files.iter().map(|file| file.slice.as_str()).collect();
    let mut found = BTreeSet::new();
    for file in files.iter().filter(|file| file.relative == "lib.rs") {
        let toks = &file.toks;
        for (i, tok) in toks.iter().enumerate() {
            let is_row = tok.is("CommandSpec")
                && toks.get(i + 1).is_some_and(|t| t.is("{"))
                && i > 0
                && (toks[i - 1].is("[") || toks[i - 1].is(","));
            if !is_row {
                continue;
            }
            let row = &toks[i + 1..=matching(toks, i + 1)];
            let names_flag = row
                .iter()
                .any(|t| is_recording_flag(t) || (t.is_ident() && constants.contains(&t.text)));
            let handler = row
                .windows(2)
                .position(|pair| pair[0].is("handler") && pair[1].is(":"));
            let Some(handler) = handler else { continue };
            let slice = row[handler + 2..]
                .iter()
                .take_while(|t| !t.is(","))
                .find(|t| t.is_ident() && known.contains(t.text.as_str()));
            if let (true, Some(slice)) = (names_flag, slice) {
                found.insert(slice.text.clone());
            }
        }
    }
    found
}

/// What the contract found in a set of source files.
#[derive(Debug, Default)]
struct Report {
    /// Recorded slice -> the signals that put it in scope.
    scope: BTreeMap<String, BTreeSet<&'static str>>,
    /// Slices with a flag, command or type signal that never import the module.
    unadopted: Vec<String>,
    /// `file: helper` for a shared helper defined in a recorded slice.
    helpers: Vec<String>,
    /// `file:line: source` for an unmarked shape read in a recorded slice.
    reads: Vec<String>,
    /// `file:line: source` for an unmarked alias of the JSON value type in a
    /// recorded slice (#4775).
    aliases: Vec<String>,
    /// `file:line: source` for a marker with a blank or placeholder reason in
    /// a recorded slice (#4773).
    markers: Vec<String>,
}

impl Report {
    fn is_clean(&self) -> bool {
        self.unadopted.is_empty()
            && self.helpers.is_empty()
            && self.reads.is_empty()
            && self.aliases.is_empty()
            && self.markers.is_empty()
    }
}

fn analyse(files: &[SourceFile]) -> Report {
    let parsed: Vec<Parsed> = files.iter().map(parse).collect();
    let constants = flag_constants(&parsed);
    let types = json_type_names(&parsed);
    let mut report = Report::default();
    let mut add = |slice: &str, signal: &'static str| {
        report
            .scope
            .entry(slice.to_string())
            .or_default()
            .insert(signal);
    };
    for file in &parsed {
        if declares_recording_flag(file) && file.slice != "lib" && file.slice != "main" {
            add(&file.slice, "flag");
        }
        if mentions_replay_type(file) {
            add(&file.slice, "type");
        }
        if imports_shared(file) {
            add(&file.slice, "import");
        }
    }
    for slice in command_slices(&parsed, &constants) {
        add(&slice, "command");
    }
    let slices: Vec<String> = report.scope.keys().cloned().collect();
    for slice in slices {
        let in_slice = || parsed.iter().filter(|file| file.slice == slice);
        let signals = &report.scope[&slice];
        let needs_module = signals.iter().any(|signal| *signal != "import");
        if needs_module && !in_slice().any(imports_shared) {
            report.unadopted.push(slice.clone());
        }
        for file in in_slice() {
            for helper in redefined_helpers(file) {
                report
                    .helpers
                    .push(format!("{}: fn {helper}", file.relative));
            }
            let findings = file_findings(file, &types);
            report.reads.extend(findings.reads);
            report.aliases.extend(findings.aliases);
            report.markers.extend(findings.markers);
        }
    }
    report
}

// ------------------------------------------------------ the real sources

#[test]
fn the_known_recorded_slices_are_in_scope() {
    let report = analyse(&sources());
    for slice in ["ai_models", "bedrock_agent"] {
        let signals = report.scope.get(slice);
        assert!(
            signals.is_some_and(|s| s.contains("flag") && s.contains("import")),
            "{slice} is no longer detected as a recorded slice: {:?}",
            report.scope
        );
    }
}

#[test]
fn every_recorded_command_is_routed_to_a_detected_slice() {
    // The COMMANDS rows that accept `--recorded` must resolve to a slice; if
    // the routing table changes shape and the resolution silently finds
    // nothing, the scope would shrink without a failure.
    let report = analyse(&sources());
    for slice in ["ai_models", "bedrock_agent"] {
        assert!(
            report.scope[slice].contains("command"),
            "no COMMANDS row naming a recording flag routes to {slice}: {:?}",
            report.scope
        );
    }
}

#[test]
fn every_recorded_slice_uses_the_shared_module() {
    let report = analyse(&sources());
    assert!(
        report.unadopted.is_empty(),
        "these slices accept a recording or define a replay client without `{SHARED_CRATE}::\
         {SHARED_MODULE}` (#4727): {:?}. Parse steps with `step_fields`, replay them with \
         `Step`/`StepQueue` and read members with the shared accessors; see the README \
         section on slices without a live AWS client.",
        report.unadopted
    );
}

#[test]
fn recorded_slices_define_no_helper_of_their_own() {
    let report = analyse(&sources());
    assert!(
        report.helpers.is_empty(),
        "recorded slices must use the `{SHARED_CRATE}::{SHARED_MODULE}` helpers instead of \
         their own copies (#4727):\n{}",
        report.helpers.join("\n")
    );
}

#[test]
fn recorded_slices_read_members_through_the_shared_helpers() {
    let report = analyse(&sources());
    assert!(
        report.reads.is_empty(),
        "JSON shape read in a recorded slice (#4727):\n{}\n\
         Read an optional response member with `{SHARED_CRATE}::{SHARED_MODULE}::optional_list`, \
         `optional_object` or `optional_text` (absent or [] is empty; null or another type is \
         an invalid response). If the value is not a response, or the member is required and \
         every other shape is rejected, put `// {MARKER} <reason>` on the line or above the item.",
        report.reads.join("\n")
    );
}

// ------------------------------------------------- negative and unit cases

fn file(relative: &str, text: &str) -> SourceFile {
    SourceFile {
        relative: relative.into(),
        text: text.into(),
    }
}

fn analyse_one(relative: &str, text: &str) -> Report {
    analyse(&[file(relative, text)])
}

/// The reviewer reproduction of #4751: a renamed accessor that maps present
/// `null` and wrong types to an empty list, behind a nested match.
const RENAMED_HELPER: &str = r#"
use toolkit_aws::recorded::Step;
struct RecordedClient;
fn response_items(response: &Json) -> &[Json] {
    match response.get("Items") {
        Some(value) => match value {
            Json::Array(items) => items,
            _ => &[],
        },
        None => &[],
    }
}
"#;

/// The reviewer reproduction of #4752: a differently named client that does
/// not use the shared module.
const OTHER_CLIENT: &str = r#"struct RecordedAwsClient;
fn items(response: &Json) -> &[Json] { match response.get("Items") { Some(Json::Array(values)) => values, _ => &[] } }"#;

#[test]
fn a_renamed_helper_with_a_nested_match_is_refused() {
    let report = analyse_one("ai_models/responses.rs", RENAMED_HELPER);
    assert!(report.scope.contains_key("ai_models"));
    assert!(report.helpers.is_empty(), "no helper name matches");
    assert_eq!(
        report.reads,
        vec!["ai_models/responses.rs:7: Json::Array(items) => items,"]
    );
}

#[test]
fn a_client_with_another_name_is_in_scope_and_must_adopt_the_module() {
    let report = analyse_one("new_slice/recording.rs", OTHER_CLIENT);
    assert_eq!(report.scope["new_slice"], BTreeSet::from(["type"]));
    assert_eq!(report.unadopted, vec!["new_slice"]);
    assert_eq!(report.reads.len(), 1, "{:?}", report.reads);
    for name in ["ReplayClient", "RecordedAwsClient", "ReplayQueue"] {
        let text = format!("pub struct {name};");
        let report = analyse_one("fresh/client.rs", &text);
        assert_eq!(report.unadopted, vec!["fresh"], "{name}");
    }
    // A trait counts too.
    let report = analyse_one("fresh/client.rs", "pub trait RecordedStepSource {}");
    assert_eq!(report.unadopted, vec!["fresh"]);
}

#[test]
fn a_recording_flag_puts_a_slice_in_scope_without_any_client_type() {
    let report = analyse_one(
        "fresh/mod.rs",
        "pub const VALUE_FLAGS: &[&str] = &[\"--region\", \"--recorded\"];",
    );
    assert_eq!(report.scope["fresh"], BTreeSet::from(["flag"]));
    assert_eq!(report.unadopted, vec!["fresh"]);
}

#[test]
fn a_command_row_naming_a_recording_flag_routes_the_scope() {
    let lib = "
pub const COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        path: &[\"skills\", \"list\"],
        flags: &[],
        value_flags: &[],
        handler: skills_list,
    },
    CommandSpec {
        path: &[\"fresh\", \"run\"],
        flags: &[],
        value_flags: &[\"--recorded\"],
        handler: fresh::run_command,
    },
    CommandSpec {
        path: &[\"other\", \"run\"],
        flags: &[],
        value_flags: other::RUN_VALUE_FLAGS,
        handler: other::run_command,
    },
    CommandSpec {
        path: &[\"quiet\", \"run\"],
        flags: &[],
        value_flags: quiet::QUIET_FLAGS,
        handler: quiet::run_command,
    },
];";
    let report = analyse(&[
        file("lib.rs", lib),
        file("fresh/mod.rs", "pub fn run_command() {}"),
        // The constant lives in another module than the handler.
        file(
            "flags.rs",
            "pub const RUN_VALUE_FLAGS: &[&str] = &[\"--recorded\"];",
        ),
        file("other/mod.rs", "pub fn run_command() {}"),
        file("quiet/mod.rs", "pub fn run_command() {}"),
    ]);
    assert_eq!(report.scope["fresh"], BTreeSet::from(["command"]));
    assert_eq!(report.scope["flags"], BTreeSet::from(["flag"]));
    assert!(!report.scope.contains_key("lib"), "{:?}", report.scope);
    assert!(!report.scope.contains_key("quiet"), "{:?}", report.scope);
    // A row that names a constant holding the flag counts, wherever it lives.
    assert_eq!(report.scope["other"], BTreeSet::from(["command"]));
    assert!(report.unadopted.contains(&"fresh".to_string()));
}

#[test]
fn every_import_form_of_the_shared_module_is_detected() {
    let forms = [
        "use toolkit_aws::recorded;",
        "use toolkit_aws::recorded::Step;",
        "use toolkit_aws::{recorded, AwsError};",
        "use toolkit_aws::{AwsError, recorded::{Step, StepQueue}};",
        "use toolkit_aws::recorded as rec;",
        "use toolkit_aws as aws; fn f() -> aws::recorded::Step { todo!() }",
        "use toolkit_aws::{self as aws, AwsError}; fn f(_: aws::recorded::Step) {}",
        "extern crate toolkit_aws as ext; fn f(_: ext::recorded::Step) {}",
        "use ::toolkit_aws::recorded::Step;",
        "mod inner { pub mod deeper { use toolkit_aws::recorded::Step; } }",
        "fn f() { let _ = toolkit_aws::recorded::same_json; }",
    ];
    for form in forms {
        let report = analyse_one("fresh/deep/er/imp.rs", form);
        assert_eq!(report.scope["fresh"], BTreeSet::from(["import"]), "{form}");
        assert!(report.unadopted.is_empty(), "{form}");
    }
    for form in [
        "use toolkit_aws::{AwsError, ServiceError};",
        "use other::recorded::Step;",
        "// use toolkit_aws::recorded::Step;",
        "const S: &str = \"use toolkit_aws::recorded::Step;\";",
        "fn recorded() {}",
    ] {
        let report = analyse_one("fresh/imp.rs", form);
        assert!(report.scope.is_empty(), "{form}: {:?}", report.scope);
    }
}

#[test]
fn an_importing_slice_still_may_not_read_shapes_or_redefine_helpers() {
    for form in [
        "use toolkit_aws::{recorded, AwsError};",
        "use toolkit_aws::recorded as rec;",
        "use toolkit_aws as aws; type S = aws::recorded::Step;",
    ] {
        let text = format!(
            "{form}\nfn list(r: &Json) -> Vec<Json> {{\n    if let Some(Json::Array(v)) = r.get(\"A\") {{ v.clone() }} else {{ vec![] }}\n}}\nfn optional_list() {{}}\n"
        );
        let report = analyse_one("fresh/deeper/nest.rs", &text);
        assert!(report.unadopted.is_empty(), "{form}");
        assert_eq!(report.reads.len(), 1, "{form}: {:?}", report.reads);
        assert_eq!(
            report.helpers,
            vec!["fresh/deeper/nest.rs: fn optional_list"]
        );
    }
}

#[test]
fn shape_reads_are_found_whatever_the_syntax() {
    let cases = [
        "fn f(v: &Json) { if let Json::Array(a) = v {} }",
        "fn f(v: &Json) { let Json::Object(o) = v else { return }; }",
        "fn f(v: &Json) { match v { Json::Object(_) | Json::Array(_) => {} _ => {} } }",
        "fn f(v: &Json) -> bool { matches!(v, Json::Array(_)) }",
        "fn f(v: &Json) { match v.get(\"A\") { Some(x) => match x { Json::Array(_) => {}, _ => {} }, None => {} } }",
        "fn f(v: &Value) { if let Value::Array(a) = v {} }",
        "fn f(v: &Value) { let _ = v.as_array(); }",
        "fn f(v: &Value) { let _ = v.as_object(); }",
        "fn f(v: &mut Value) { let _ = v.as_array_mut(); }",
        "fn f(v: &mut Value) { let _ = v.as_object_mut(); }",
        "fn f(v: Option<&Value>) { let _ = v.and_then(Value::as_array); }",
        "fn f(v: &toolkit_core::Json) { if let toolkit_core::Json::Array(a) = v {} }",
        "use toolkit_core::Json as J;\nfn f(v: &J) { if let J::Array(a) = v {} }",
        "use toolkit_core::Json::{Array, Object};\nfn f(v: &Json) { if let Array(a) = v {} }",
        "use toolkit_core::Json::*;\nfn f(v: &Json) { if let Object(o) = v {} }",
        "use toolkit_core::Json::Array as List;\nfn f(v: &Json) { if let List(a) = v {} }",
        "fn f(v: &Json) { if let Json\n    ::\n    Array(a) = v {} }",
    ];
    for case in cases {
        let text = format!("use toolkit_aws::recorded::Step;\n{case}");
        let report = analyse_one("fresh/x.rs", &text);
        assert!(!report.reads.is_empty(), "not flagged: {case}");
    }
}

#[test]
fn code_that_is_not_a_shape_read_is_not_flagged() {
    let source = "use toolkit_aws::recorded::Step;
// Json::Array(in a comment) and as_array too
/// A doc line: `Some(Json::Object(_))`.
/* a block /* nested Json::Array */ comment .as_object() */
fn f(v: &Json) -> usize {
    let s = \"Json::Array(x) .as_array()\";
    let raw = r#\"Value::Object(\"#;
    let c = '\\'';
    let lifetime: &'static str = \"a\";
    let list: Vec<Json> = Vec::new();
    let named = Json::object(vec![]);
    let n = Json::Null;
    let built = Json::Array(vec![Json::Object(Vec::new())]);
    let pair = vec![(\"k\".to_string(), Json::Array(list.clone()))];
    let mapped: Vec<Json> = pair.iter().map(|(_, v)| v.clone()).chain(list.into_iter().map(Json::Array)).collect();
    let same = v == &Json::Array(Vec::new());
    let ok: Result<Json, ()> = Ok(Json::Object(Vec::new()));
    let a_array = array_len(v);
    list.len() + s.len() + raw.len() + lifetime.len()
}
#[cfg(test)]
mod tests {
    fn t() { let _ = Json::Array(Vec::new()); let _ = v.as_array(); }
}
";
    let report = analyse_one("fresh/ok.rs", source);
    assert!(report.is_clean(), "{report:?}");
}

#[test]
fn the_marker_covers_its_line_or_the_item_under_it() {
    let source = "use toolkit_aws::recorded::Step;
fn f(response: &Json) {
    // recorded-shape: a required member, every other shape is rejected.
    let Some(Json::Object(m)) = response.get(\"M\") else { return };
    // recorded-shape: a comment block
    // that spans two lines.
    let Some(Json::Array(n)) = response.get(\"N\") else { return };
    let Some(Json::Array(o)) = response.get(\"O\") else { return }; // recorded-shape: inline, a required member
    // recorded-shape: only covers the next code line.
    let a = 1;
    let Some(Json::Object(p)) = response.get(\"P\") else { return };

    // recorded-shape: a blank line separates this from the code.

    let Some(Json::Object(q)) = response.get(\"Q\") else { return };
}
// recorded-shape: the whole item, the manifest is the command's own file.
#[allow(dead_code)]
fn manifest(value: &Json) -> usize {
    match value {
        Json::Array(items) => items.len(),
        Json::Object(pairs) => pairs.len(),
        _ => 0,
    }
}
/// Docs first.
// recorded-shape: a constant this module builds.
pub const SHAPES: [fn(&Json) -> bool; 1] = [|j| matches!(j, Json::Array(_))];
fn after(value: &Json) -> bool {
    matches!(value, Json::Array(_))
}
";
    let report = analyse_one("fresh/marked.rs", source);
    let lines: Vec<&str> = report
        .reads
        .iter()
        .map(|read| read.split(':').nth(1).expect("line"))
        .collect();
    assert_eq!(lines, vec!["11", "15", "30"], "{:?}", report.reads);
}

#[test]
fn the_lexer_blanks_comments_and_literals_and_keeps_lines() {
    let mut lexed = lex("let a = \"x\ny\"; // c1\n/* b1\nb2 */ let r = r##\"q\"#\"##; let l = 'a'; 'life: loop {}\n");
    assert_eq!(lexed.code.lines().count(), 4);
    assert!(!lexed.code.contains("c1") && !lexed.code.contains("b1"));
    assert!(lexed.comments[1].contains("c1"));
    assert!(lexed.comments[2].contains("b1"));
    let toks = tokenize(&mut lexed);
    let strings: Vec<&str> = toks
        .iter()
        .filter(|t| t.is_str)
        .map(|t| t.text.as_str())
        .collect();
    assert_eq!(strings, vec!["x\ny", "q\"#"]);
    assert!(toks.iter().any(|t| t.is("loop") && t.line == 3));
}

#[test]
fn the_helper_check_is_an_extra_signal_for_the_known_names() {
    let source = "use toolkit_aws::recorded::Step;
fn optional_member<'j, T>() {}
pub(crate) fn same_json(a: &Json, b: &Json) -> bool { a == b }
// fn optional_text(...) in a comment is not a definition
fn optional_list_len() {}
";
    let report = analyse_one("fresh/h.rs", source);
    assert_eq!(
        report.helpers,
        vec!["fresh/h.rs: fn optional_member", "fresh/h.rs: fn same_json"]
    );
    assert_eq!(slice_of("ai_models/hub.rs"), "ai_models");
    assert_eq!(slice_of("bedrock_agent.rs"), "bedrock_agent");
    assert!(is_test_file("ai_models/tests/hub.rs"));
    assert!(is_test_file("bedrock_agent/tests.rs"));
    assert!(!is_test_file("ai_models/hub.rs"));
}

#[test]
fn a_slice_with_no_recording_signal_is_not_scanned() {
    let source = "fn f(v: &Json) { if let Json::Array(a) = v {} let _ = v.as_object(); }";
    let report = analyse_one("plain/mod.rs", source);
    assert!(report.scope.is_empty());
    assert!(report.is_clean());
}

#[test]
fn every_evasion_of_the_first_scanner_fails_the_whole_contract() {
    // The review (#4745) fixtures and their relatives: none of them was
    // refused by the spelling-based scanner of 8f429632.
    let read = "fn items(r: &Json) -> &[Json] { match r.get(\"I\") { Some(Json::Array(v)) => v, _ => &[] } }";
    let cases: Vec<(&str, String)> = vec![
        ("ai_models/responses.rs", RENAMED_HELPER.to_string()),
        ("new_slice/recording.rs", OTHER_CLIENT.to_string()),
        (
            "fresh/client.rs",
            format!("pub struct ReplayClient;\n{read}"),
        ),
        (
            "fresh/mod.rs",
            format!("pub const F: &[&str] = &[\"--recorded\"];\n{read}"),
        ),
        (
            "fresh/a.rs",
            format!("use toolkit_aws::{{recorded, AwsError}};\n{read}"),
        ),
        (
            "fresh/a.rs",
            format!("use toolkit_aws as aws;\ntype S = aws::recorded::Step;\n{read}"),
        ),
        (
            "fresh/a.rs",
            format!("use toolkit_aws::recorded as rec;\n{read}"),
        ),
        (
            "fresh/deep/er/a.rs",
            format!("mod inner {{ use toolkit_aws::{{self as a, recorded::Step}}; }}\n{read}"),
        ),
        (
            "fresh/a.rs",
            "use toolkit_aws::recorded::Step;\nfn xs(v: &Json) -> usize { if let Json::Array(a) = v { a.len() } else { 0 } }"
                .to_string(),
        ),
    ];
    for (relative, text) in cases {
        let report = analyse_one(relative, &text);
        assert!(
            !report.is_clean(),
            "accepted: {relative}\n{text}\n{report:?}"
        );
    }
}

#[test]
fn the_hand_written_helpers_and_reads_of_the_first_clients_are_refused() {
    // The positive control of #4727: the eight helper copies of the clients
    // before they moved onto the shared module, and their two direct reads.
    let source = "\
struct RecordedClient;
fn optional_member<'j, T>(o: &'j Json, k: &str, e: &str, f: impl FnOnce(&'j Json) -> Option<T>) {}
fn optional_list(o: &Json, k: &str) {}
fn optional_object(o: &Json, k: &str) {}
fn optional_object_list(o: &Json, k: &str) {}
fn optional_string_list(o: &Json, k: &str) {}
fn optional_text(o: &Json, k: &str) {}
fn nullable_text(o: &Json, k: &str) {}
fn same_json(a: &Json, b: &Json) -> bool { a == b }
fn schema(api: &Json) {
    let Some(s3_ref @ Json::Object(_)) = api.get(\"s3\") else { return };
    if let Some(Json::Array(aliases)) = api.get(\"aliases\") {}
}
";
    let report = analyse_one("old_slice/client.rs", source);
    assert_eq!(report.helpers.len(), 8, "{:?}", report.helpers);
    assert_eq!(report.reads.len(), 2, "{:?}", report.reads);
    assert_eq!(report.unadopted, vec!["old_slice"]);
}

// ------------------------------------- review #4745 round 2 (#4773-#4775)

#[test]
fn recorded_slices_alias_no_json_type_and_give_every_marker_a_reason() {
    let report = analyse(&sources());
    assert!(
        report.aliases.is_empty() && report.markers.is_empty(),
        "recorded slices (#4727) must not alias the JSON value type (#4775):\n{}\n\
         and every `// {MARKER} <reason>` needs a specific reason (#4773):\n{}",
        report.aliases.join("\n"),
        report.markers.join("\n")
    );
}

/// The read every round-2 fixture hides: a `null` or wrong-typed `Items`
/// defaults to an empty list.
const ITEMS_READ: &str =
    "fn items(r: &Json) -> &[Json] {\n    match r.get(\"Items\") { Some(Json::Array(v)) => v, _ => &[] }\n}";

/// Every round-2 evasion: each passes the scanner of 44c513a5 and fails this
/// one. `(label, files)`; the in-scope slice is `fresh`. In order: markers
/// (#4773, 0..12), patterns (#4774, 12..21), aliases (#4775, 21..).
fn second_review_evasions() -> Vec<(&'static str, Vec<SourceFile>)> {
    let mut cases = marker_evasions();
    cases.extend(pattern_evasions());
    cases.extend(alias_evasions());
    cases
}

/// #4773: blank or placeholder reasons and module-wide exemptions.
fn marker_evasions() -> Vec<(&'static str, Vec<SourceFile>)> {
    let one = |text: String| vec![file("fresh/deep/recording.rs", &text)];
    let use_line = "use toolkit_aws::recorded::Step;";
    let marked = |marker: &str| one(format!("{use_line}\n{marker}\n{ITEMS_READ}"));
    vec![
        // #4773: blank or placeholder reasons.
        (
            "blank reason",
            one(format!(
                "{use_line}\nfn items(r: &Json) -> &[Json] {{\n    // recorded-shape:\n    match r.get(\"Items\") {{ Some(Json::Array(v)) => v, _ => &[] }}\n}}"
            )),
        ),
        ("blank reason above the item", marked("// recorded-shape:")),
        ("whitespace reason", marked("// recorded-shape:    \t")),
        ("todo reason", marked("// recorded-shape: todo")),
        ("x reason", marked("// recorded-shape: x")),
        ("placeholder reason", marked("// recorded-shape: <reason>")),
        ("todo fix later", marked("// recorded-shape: TODO fix later")),
        // #4773: module-wide exemptions.
        (
            "marker above a mod",
            one(format!(
                "{use_line}\n// recorded-shape: module header for the snapshot readers\nmod nested {{\n    fn items(r: &Json) -> &[Json] {{\n        match r.get(\"Items\") {{ Some(Json::Array(v)) => v, _ => &[] }}\n    }}\n    fn objects(r: &Json) -> bool {{ matches!(r, Json::Object(_)) }}\n}}"
            )),
        ),
        (
            "marker above an impl block",
            one(format!(
                "{use_line}\nstruct Holder;\n// recorded-shape: the client this module builds for itself\nimpl Holder {{\n    fn items(&self, r: &Json) -> usize {{\n        match r.get(\"Items\") {{ Some(Json::Array(v)) => v.len(), _ => 0 }}\n    }}\n}}"
            )),
        ),
        (
            "marker above a trait block",
            one(format!(
                "{use_line}\n// recorded-shape: default methods over a snapshot file\ntrait Reads {{\n    fn items(&self, r: &Json) -> usize {{\n        match r.get(\"Items\") {{ Some(Json::Array(v)) => v.len(), _ => 0 }}\n    }}\n}}"
            )),
        ),
        (
            "file header above an inner attribute",
            one(format!(
                "// recorded-shape: every value in this file is a snapshot file\n#![allow(dead_code)]\n{ITEMS_READ}\n{use_line}"
            )),
        ),
        (
            "trailing marker on a function signature",
            one(format!(
                "{use_line}\nfn items(r: &Json) -> &[Json] {{ // recorded-shape: a required member of the snapshot\n    match r.get(\"Items\") {{ Some(Json::Array(v)) => v, _ => &[] }}\n}}"
            )),
        ),
    ]
}

/// #4774: brace-form and struct-field patterns.
fn pattern_evasions() -> Vec<(&'static str, Vec<SourceFile>)> {
    let one = |text: String| vec![file("fresh/deep/recording.rs", &text)];
    let use_line = "use toolkit_aws::recorded::Step;";
    vec![
        (
            "brace-form tuple variant",
            one(format!(
                "{use_line}\nfn items(r: &Json) -> &[Json] {{\n    match r.get(\"Items\") {{ Some(Json::Array {{ 0: v }}) => v, _ => &[] }}\n}}"
            )),
        ),
        (
            "brace-form rest pattern",
            one(format!(
                "{use_line}\nfn kind(v: &Json) -> u8 {{ match v {{ Json::Object {{ .. }} => 1, _ => 0 }} }}"
            )),
        ),
        (
            "brace-form if let with a ref binding",
            one(format!(
                "{use_line}\nfn n(v: &Value) -> usize {{ if let Value::Array {{ 0: ref items }} = v {{ items.len() }} else {{ 0 }} }}"
            )),
        ),
        (
            "brace-form let else",
            one(format!(
                "{use_line}\nfn n(r: &Json) -> usize {{ let Some(Json::Object {{ 0: m }}) = r.get(\"M\") else {{ return 0 }}; m.len() }}"
            )),
        ),
        (
            "brace-form in matches!",
            one(format!(
                "{use_line}\nfn is_list(r: &Json) -> bool {{ matches!(r, Json::Array {{ .. }}) }}"
            )),
        ),
        (
            "brace-form or-pattern in matches!",
            one(format!(
                "{use_line}\nfn ok(r: &Json) -> bool {{ matches!(r.get(\"A\"), Some(Json::Object {{ .. }}) | None) }}"
            )),
        ),
        (
            "brace-form with a guard",
            one(format!(
                "{use_line}\nfn n(r: &Json) -> usize {{ match r.get(\"M\") {{ Some(Json::Object {{ 0: m }}) if !m.is_empty() => m.len(), _ => 0 }} }}"
            )),
        ),
        (
            "variant in a struct-field pattern",
            one(format!(
                "{use_line}\nstruct Holder<'a> {{ body: &'a Json }}\nfn n(h: Holder) -> usize {{ match h {{ Holder {{ body: Json::Array(v), .. }} => v.len(), _ => 0 }} }}"
            )),
        ),
        (
            "variant in a brace-form Some",
            one(format!(
                "{use_line}\nfn n(r: &Json) -> usize {{ match r.get(\"I\") {{ Some {{ 0: Json::Array(v) }} => v.len(), _ => 0 }} }}"
            )),
        ),
    ]
}

/// #4775: aliases of the JSON value type.
fn alias_evasions() -> Vec<(&'static str, Vec<SourceFile>)> {
    let one = |text: String| vec![file("fresh/deep/recording.rs", &text)];
    let use_line = "use toolkit_aws::recorded::Step;";
    vec![
        (
            "type alias",
            one(format!(
                "{use_line}\ntype J = Json;\nfn items(r: &J) -> &[J] {{\n    match r.get(\"Items\") {{ Some(J::Array(v)) => v, _ => &[] }}\n}}"
            )),
        ),
        (
            "qualified serde_json type alias",
            one(format!(
                "{use_line}\ntype V = serde_json::Value;\nfn n(r: &V) -> usize {{ if let Some(V::Object(m)) = r.get(\"M\") {{ m.len() }} else {{ 0 }} }}"
            )),
        ),
        (
            "public type alias with a leading ::",
            one(format!("{use_line}\npub(crate) type Doc = ::toolkit_core::Json;")),
        ),
        (
            "use as alias",
            one(format!("{use_line}\nuse serde_json::Value as V;")),
        ),
        (
            "grouped use as alias",
            one(format!("{use_line}\nuse toolkit_core::{{Json as J, Other}};")),
        ),
        (
            "re-export alias",
            one(format!("{use_line}\npub use toolkit_core::Json as Doc;")),
        ),
        (
            "alias of a marked alias",
            one(format!(
                "{use_line}\n// recorded-shape: the alias keeps the name of the Python port\ntype J = Json;\ntype K = J;\nfn n(r: &K) -> usize {{ match r.get(\"I\") {{ Some(K::Array(v)) => v.len(), _ => 0 }} }}"
            )),
        ),
        (
            "alias re-exported from another module",
            vec![
                file("util.rs", "pub use toolkit_core::Json as Doc;"),
                file(
                    "fresh/a.rs",
                    &format!(
                        "{use_line}\nuse crate::util::Doc;\nfn n(r: &Doc) -> usize {{ match r.get(\"I\") {{ Some(Doc::Array(v)) => v.len(), _ => 0 }} }}"
                    ),
                ),
            ],
        ),
    ]
}

#[test]
fn every_round_two_evasion_fails_the_whole_contract() {
    for (label, files) in second_review_evasions() {
        let report = analyse(&files);
        assert!(
            report.scope.contains_key("fresh"),
            "{label}: {:?}",
            report.scope
        );
        assert!(!report.is_clean(), "accepted: {label}\n{report:?}");
    }
}

#[test]
fn a_marker_needs_a_specific_reason() {
    for (reason, specific) in [
        ("", false),
        ("   ", false),
        ("todo", false),
        ("x", false),
        ("<reason>", false),
        ("TODO fix later", false),
        ("a b c d", false),
        ("ok ok ok", false),
        ("the manifest this command wrote", true),
        ("a required member; every other shape is rejected", true),
        ("the Search request expression, as above.", true),
    ] {
        assert_eq!(is_specific_reason(reason), specific, "{reason:?}");
    }
    let report = analyse(&second_review_evasions()[0].1);
    assert_eq!(report.markers.len(), 1, "{report:?}");
    assert_eq!(
        report.reads.len(),
        1,
        "a refused marker allows nothing: {report:?}"
    );
    // A reason may continue on the comment lines under the marker.
    let source = "use toolkit_aws::recorded::Step;
fn f(r: &Json) -> usize {
    // recorded-shape: a snapshot
    // file the user supplied.
    let Some(Json::Array(v)) = r.get(\"A\") else { return 0 };
    v.len()
}
";
    assert!(analyse_one("fresh/m.rs", source).is_clean());
}

#[test]
fn a_marker_covers_a_function_method_or_closure_never_a_module_or_block() {
    let source = "use toolkit_aws::recorded::Step;
struct Holder;
impl Holder {
    // recorded-shape: the document object this command assembles itself.
    fn method(&self, r: &Json) -> usize {
        match r { Json::Array(v) => v.len(), Json::Object(m) => m.len(), _ => 0 }
    }
    fn unmarked(&self, r: &Json) -> bool { matches!(r, Json::Array(_)) }
}
fn g(r: &Json) -> usize {
    // recorded-shape: a closure over the snapshot file the user supplied.
    let size = |v: &Json| match v {
        Json::Array(items) => items.len(),
        _ => 0,
    };
    let next = matches!(r, Json::Object(_));
    size(r) + usize::from(next)
}
// recorded-shape: a constant table built by this module, not a response.
const KINDS: [fn(&Json) -> bool; 2] = [
    |j| matches!(j, Json::Array(_)),
    |j| matches!(j, Json::Object(_)),
];
";
    let report = analyse_one("fresh/leaf.rs", source);
    let lines: Vec<&str> = report
        .reads
        .iter()
        .map(|read| read.split(':').nth(1).expect("line"))
        .collect();
    assert_eq!(lines, vec!["8", "16", "21", "22"], "{:?}", report.reads);
    // Every module-wide fixture leaves each read it hides refused.
    for (label, files) in &second_review_evasions()[7..12] {
        let report = analyse(files);
        assert!(!report.reads.is_empty(), "{label}: {report:?}");
        assert!(report.markers.is_empty(), "{label}: {report:?}");
    }
}

#[test]
fn brace_form_and_struct_field_patterns_are_reads_but_constructions_are_not() {
    for (label, files) in &second_review_evasions()[12..21] {
        let report = analyse(files);
        assert_eq!(report.reads.len(), 1, "{label}: {report:?}");
    }
    let source = "use toolkit_aws::recorded::Step;
struct Holder { body: Json }
fn f(list: Vec<Json>, n: bool) -> Holder {
    let built = Json::Array { 0: list.clone() };
    let held = Holder { body: Json::Array(list.clone()) };
    let picked = match n { true => Holder { body: Json::Object(Vec::new()) }, false => held };
    if n { Holder { body: built } } else { picked }
}
";
    let report = analyse_one("fresh/build.rs", source);
    assert!(report.is_clean(), "{report:?}");
}

#[test]
fn an_alias_of_the_json_type_is_refused_and_its_reads_still_count() {
    let cases = second_review_evasions();
    let by_label = |label: &str| {
        let (_, files) = cases
            .iter()
            .find(|(name, _)| *name == label)
            .expect("fixture");
        analyse(files)
    };
    for (label, aliases, reads) in [
        ("type alias", 1, 1),
        ("qualified serde_json type alias", 1, 1),
        ("public type alias with a leading ::", 1, 0),
        ("use as alias", 1, 0),
        ("grouped use as alias", 1, 0),
        ("re-export alias", 1, 0),
        // The marked `type J` is allowed; `type K = J` and the read are not.
        ("alias of a marked alias", 1, 1),
        // The re-export lives outside the slice; the read through it counts.
        ("alias re-exported from another module", 0, 1),
    ] {
        let report = by_label(label);
        assert_eq!(report.aliases.len(), aliases, "{label}: {report:?}");
        assert_eq!(report.reads.len(), reads, "{label}: {report:?}");
    }
    // Neither a type whose path merely contains `Json`, nor a cast, nor
    // `<Json as Trait>` is an alias.
    let source = "use toolkit_aws::recorded::Step;
use std::fmt::Write as _;
type Pairs = Vec<(String, Json)>;
type Handle<'h> = &'h mut dyn FnMut(&Json) -> bool;
fn f(v: &Json) -> String { <Json as ToString>::to_string(v) }
";
    let report = analyse_one("fresh/types.rs", source);
    assert!(report.is_clean(), "{report:?}");
}

// ------------------------------------- review #4745 round 3 (#4793, #4794)

/// Every round-3 evasion: each passes the scanner of ec2740ed and fails this
/// one. `(label, files)`; the in-scope slice is `fresh`. In order: annotated
/// `let` patterns (#4793, 0..5), `matches!` delimiters (#4794, 5..).
fn third_review_evasions() -> Vec<(&'static str, Vec<SourceFile>)> {
    let one = |text: String| vec![file("fresh/deep/recording.rs", &text)];
    let use_line = "use toolkit_aws::recorded::Step;";
    vec![
        (
            "annotated tuple let else",
            one(format!(
                "{use_line}\nfn items(r: &Json) -> &[Json] {{\n    let Some(Json::Array(v)): Option<&Json> = r.get(\"Items\") else {{ return &[]; }};\n    v\n}}"
            )),
        ),
        (
            "annotated brace let else",
            one(format!(
                "{use_line}\nfn items(r: &Json) -> &[Json] {{\n    let Some(Json::Array {{ 0: v }}): Option<&Json> = r.get(\"Items\") else {{ return &[]; }};\n    v\n}}"
            )),
        ),
        (
            "annotated nested let else",
            one(format!(
                "{use_line}\nfn n<'a>(r: &'a Json) -> usize {{\n    let Ok(Some(&Json::Object(ref m))): Result<Option<&'a Json>, ()> = Ok(r.get(\"M\")) else {{ return 0; }};\n    m.len()\n}}"
            )),
        ),
        (
            "annotated tuple of patterns",
            one(format!(
                "{use_line}\nfn n(r: &Json) -> usize {{\n    let (Some(Json::Array(v)), k): (Option<&Json>, fn(&Json) -> bool) = (r.get(\"I\"), |_| true) else {{ return 0; }};\n    v.len() + usize::from(k(r))\n}}"
            )),
        ),
        (
            "annotated direct let else",
            one(format!(
                "{use_line}\nfn n(r: &Json) -> usize {{\n    let &Json::Array(ref v): &Json = r else {{ return 0; }};\n    v.len()\n}}"
            )),
        ),
        (
            "annotated struct-field let else",
            one(format!(
                "{use_line}\nstruct Holder<'a> {{ body: &'a Json }}\nfn n(h: Holder) -> usize {{\n    let Holder {{ body: Json::Array(v) }}: Holder = h else {{ return 0; }};\n    v.len()\n}}"
            )),
        ),
        (
            "matches! with braces",
            one(format!(
                "{use_line}\nfn objects(r: &Json) -> bool {{ matches! {{ r, Json::Object(_) }} }}"
            )),
        ),
        (
            "matches! with brackets",
            one(format!(
                "{use_line}\nfn objects(r: &Json) -> bool {{ matches![r.get(\"A\"), Some(Json::Array(_))] }}"
            )),
        ),
    ]
}

#[test]
fn every_round_three_evasion_fails_the_whole_contract() {
    for (label, files) in third_review_evasions() {
        let report = analyse(&files);
        assert!(
            report.scope.contains_key("fresh"),
            "{label}: {:?}",
            report.scope
        );
        assert!(!report.is_clean(), "accepted: {label}\n{report:?}");
        assert_eq!(report.reads.len(), 1, "{label}: {report:?}");
    }
}

#[test]
fn annotated_lets_and_every_matches_delimiter_are_reads_but_constructions_are_not() {
    // The marked positive control: a required member, every other shape
    // rejected, with the annotation an author adds for documentation.
    let marked = "use toolkit_aws::recorded::Step;
fn name(r: &Json) -> Result<&str, String> {
    // recorded-shape: a required member of the snapshot; every other shape is rejected.
    let Some(Json::Object(m)): Option<&Json> = r.get(\"Doc\") else {
        return Err(\"Doc must be an object\".into());
    };
    Ok(m.name())
}
";
    let report = analyse_one("fresh/marked.rs", marked);
    assert!(report.is_clean(), "{report:?}");
    // The same lines without the marker are one read.
    let unmarked = marked.replace(
        "    // recorded-shape: a required member of the snapshot; every other shape is rejected.\n",
        "",
    );
    let report = analyse_one("fresh/marked.rs", &unmarked);
    assert_eq!(report.reads.len(), 1, "{report:?}");
    // Building a value is not a read, whatever the annotation around it.
    let source = "use toolkit_aws::recorded::Step;
struct Holder { body: Json }
fn f(list: Vec<Json>, pairs: Vec<(String, Json)>) -> Holder {
    let built: Json = Json::Array(list.clone());
    let braced: Json = Json::Array { 0: list.clone() };
    let some: Option<Json> = Some(Json::Object(pairs.clone()));
    let nested: Result<Option<Json>, ()> = Ok(Some(Json::Array { 0: list.clone() }));
    let held: Holder = Holder { body: Json::Array(list) };
    let table: [(&str, fn(&Json) -> Json); 1] = [(\"a\", |_| Json::Object(Vec::new()))];
    let same: bool = held.body == built && some.is_some() && nested.is_ok() && table.len() == 1;
    let _: Json = braced;
    let _ = same;
    held
}
fn g(list: Vec<Json>) -> Json {
    let tuple: (Json, usize) = (Json::Array(list), 1);
    tuple.0
}
";
    let report = analyse_one("fresh/build.rs", source);
    assert!(report.is_clean(), "{report:?}");
}

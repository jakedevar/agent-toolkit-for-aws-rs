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
//! 2. it reads no JSON shape: any mention of `Json::Array`, `Json::Object`,
//!    `Value::Array` or `Value::Object` (patterns in `match`, `if let`,
//!    `let ... else`, `matches!`, and constructions alike; also through an
//!    alias or a `use Json::{Array, Object}` import), and any call or path
//!    `as_array`, `as_object`, `as_array_mut`, `as_object_mut`, is refused
//!    unless the line, or the item directly under the comment, carries the
//!    marker `// recorded-shape: <reason>`. A marker above an item (`fn`,
//!    `impl`, `const`, ...) covers the whole item; above a statement it covers
//!    that line. The reason says why the value is not a response member (a
//!    snapshot file, the command's own manifest, a value the slice built) or
//!    why the member is required and every other shape is rejected.
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
/// Keywords that start an item.
const ITEM_KEYWORDS: [&str; 12] = [
    "fn",
    "impl",
    "mod",
    "trait",
    "const",
    "static",
    "struct",
    "enum",
    "union",
    "type",
    "macro_rules",
    "use",
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

/// Whether the tokens from `start` begin an item (after attributes and
/// visibility), as opposed to a statement.
fn starts_item(toks: &[Tok], start: usize) -> bool {
    let mut i = start;
    loop {
        match toks.get(i) {
            Some(tok) if tok.is("#") => {
                let mut open = i + 1;
                if toks.get(open).is_some_and(|t| t.is("!")) {
                    open += 1;
                }
                if !toks.get(open).is_some_and(|t| t.is("[")) {
                    return false;
                }
                i = matching(toks, open) + 1;
            }
            Some(tok) if tok.is("pub") => {
                i += 1;
                if toks.get(i).is_some_and(|t| t.is("(")) {
                    i = matching(toks, i) + 1;
                }
            }
            Some(tok) if tok.is("unsafe") || tok.is("async") || tok.is("extern") => i += 1,
            Some(tok) if tok.is("const") => {
                let next = toks.get(i + 1);
                if next.is_some_and(|t| t.is("fn") || t.is("unsafe") || t.is("async")) {
                    i += 1;
                } else {
                    return true;
                }
            }
            Some(tok) => return ITEM_KEYWORDS.iter().any(|keyword| tok.is(keyword)),
            None => return false,
        }
    }
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

/// The 0-based lines the markers of `file` allow.
fn allowed_lines(file: &Parsed) -> BTreeSet<usize> {
    let mut allowed = BTreeSet::new();
    for (marker_line, comment) in file.comments.iter().enumerate() {
        if !comment.contains(MARKER) {
            continue;
        }
        let target = if file.code_lines.get(marker_line) == Some(&true) {
            Some(marker_line)
        } else {
            // The comment block directly above a code line.
            let mut below = marker_line + 1;
            while below < file.comments.len()
                && !file.code_lines[below]
                && !file.comments[below].trim().is_empty()
            {
                below += 1;
            }
            (below < file.comments.len() && file.code_lines[below]).then_some(below)
        };
        let Some(target) = target else { continue };
        allowed.insert(target);
        if let Some(first) = file.toks.iter().position(|tok| tok.line >= target) {
            if file.toks[first].line == target && starts_item(&file.toks, first) {
                let end = item_end(&file.toks, first);
                allowed.extend(target..=file.toks[end].line);
            }
        }
    }
    allowed
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

/// Whether the expression or pattern covering `start..=end` is in a pattern
/// position: followed by `=>`, `=` (a `let`), `|` or a guard `if`, directly
/// or through the tuple-struct, tuple, slice or `matches!` pattern around it.
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
    if !(after.is(",") || after.is(")") || after.is("]")) {
        return false;
    }
    let Some(open) = enclosing_open(toks, start) else {
        return false;
    };
    if toks[open].is("{") {
        return false;
    }
    if toks[open].is("(") && open >= 2 && toks[open - 1].is("!") && toks[open - 2].is("matches") {
        // `matches!(value, PATTERN)`: everything after the first comma.
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
    in_pattern_position(toks, call_start(toks, open), matching(toks, open))
}

/// Whether the variant path ending at `variant` is matched on, as opposed to
/// built.
fn is_pattern(toks: &[Tok], variant: usize) -> bool {
    let start = path_start(toks, variant);
    if start > 0 && toks[start - 1].is("@") {
        return true;
    }
    let end = if toks.get(variant + 1).is_some_and(|t| t.is("(")) {
        matching(toks, variant + 1)
    } else {
        variant
    };
    in_pattern_position(toks, start, end)
}

/// The 0-based lines that read a JSON shape, whether allowed or not.
fn shape_reads(toks: &[Tok]) -> BTreeSet<usize> {
    let in_use = use_tokens(toks);
    let mut types: BTreeSet<&str> = SHAPE_TYPES.into_iter().collect();
    for window in toks.windows(3) {
        if SHAPE_TYPES.iter().any(|name| window[0].is(name))
            && window[1].is("as")
            && window[2].is_ident()
        {
            types.insert(&window[2].text);
        }
    }
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

/// The shape reads of `file` that no marker allows: (1-based line, source line).
fn unmarked_shape_reads(file: &Parsed) -> Vec<(usize, String)> {
    let allowed = allowed_lines(file);
    let source_lines: Vec<&str> = file.text.lines().collect();
    shape_reads(&file.toks)
        .into_iter()
        .filter(|line| !allowed.contains(line))
        .map(|line| {
            (
                line + 1,
                source_lines.get(line).unwrap_or(&"").trim().to_string(),
            )
        })
        .collect()
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
        if tok.is(SHARED_CRATE) && toks.get(i + 1).is_some_and(|t| t.is("::")) {
            if toks.get(i + 2).is_some_and(|t| t.is("{")) {
                let group = &toks[i + 2..=matching(toks, i + 2)];
                for window in group.windows(3) {
                    if window[0].is("self") && window[1].is("as") && window[2].is_ident() {
                        crates.insert(&window[2].text);
                    }
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
}

impl Report {
    fn is_clean(&self) -> bool {
        self.unadopted.is_empty() && self.helpers.is_empty() && self.reads.is_empty()
    }
}

fn analyse(files: &[SourceFile]) -> Report {
    let parsed: Vec<Parsed> = files.iter().map(parse).collect();
    let constants = flag_constants(&parsed);
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
            for (line, text) in unmarked_shape_reads(file) {
                report
                    .reads
                    .push(format!("{}:{line}: {text}", file.relative));
            }
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
    let Some(Json::Array(o)) = response.get(\"O\") else { return }; // recorded-shape: inline
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
// recorded-shape: a constant.
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

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
//! helper name, a nesting of `match`). It parses every non-test source file of
//! `crates/toolkit-cli/src` with `syn` (#4751): a pattern is found where the
//! grammar puts it, never by guessing from the tokens around it. Comments
//! (markers) and the scope signals come from a small lexer.
//!
//! **Scope** (#4752). [`INVENTORY`] classifies every top-level module of `src`
//! (the directory and its `<name>.rs`); an unclassified module fails closed.
//! `Recorded` modules get every check. `NotRecorded` modules name what they
//! read instead and must show no recording signal. `Pending` modules read
//! recorded responses by hand and wait on a follow-up Issue; their finding
//! count is pinned. The recording signals cross-check the inventory, and
//! without an inventory they alone decide the scope:
//!
//! - a recording flag: a string literal `"--recorded"` (also `"--recording"`,
//!   `"--replay"`) in the slice, or a `COMMANDS` row of `lib.rs` that names the
//!   flag, or a constant that holds it, and routes to the slice;
//! - a recorded replay type, trait or constant: a capitalised name that
//!   mentions `Recorded`, `Recording` or `Replay` (`RecordedAwsClient`,
//!   `ReplayQueue`, `RecordedPriceList`, `MAX_RECORDED_BYTES`);
//! - an import of `toolkit_aws::recorded` in any path form: plain, grouped
//!   (`toolkit_aws::{recorded, ..}`), aliased (`recorded as r`,
//!   `toolkit_aws as aws` then `aws::recorded`), in a nested module.
//!
//! A slice whose only signal is the import adopted the module already. A slice
//! with a flag, type or inventory signal and no import fails the adoption
//! check.
//!
//! **Checks** on every file of a recorded slice (#4751):
//!
//! 1. it defines none of the shared helpers (`optional_member`,
//!    `optional_list`, `optional_object`, `optional_text`, `same_json`, ...);
//! 2. it reads no JSON shape: any pattern whose path ends in `Array` or
//!    `Object` (tuple or brace form (#4774), through an alias, `Self`, an
//!    imported variant, or a name any `use` in `src` renames one to, at any
//!    path length (#4863)), in any pattern position (`match`, `if let`,
//!    `while let`, `let .. else`, an annotated `let` (#4793), a guard, a field
//!    of a struct pattern, behind `&`, `ref`, `@`, `Some` or a tuple), and any
//!    call or path `as_array`, `as_object`, `as_array_mut`, `as_object_mut`.
//!    A raw identifier is the name it spells (`r#Array`, #4861). `matches!`
//!    (any delimiter, #4794) and the standard expression macros are parsed;
//!    any other macro body fails closed (a shape variant with a payload
//!    counts, even when built). Building a value in code is not a read;
//! 3. it does not alias the JSON value type (#4775): `type J = Json;`,
//!    `type J = (Json);`, `type V = serde_json::Value;`, `use ..::Json as J`,
//!    `use ..::Json::{self as J}`, a re-export, an alias of an alias. A marked
//!    alias is allowed but its reads still count, and an alias defined
//!    anywhere in `src` is followed into the slice;
//! 4. every marker `// recorded-shape: <reason>` has a specific reason
//!    (#4773): at least three distinct words, placeholders (`todo`, `x`,
//!    `<reason>`) and filler (`this is fine, see above`) not counted. A
//!    refused marker allows nothing. A marker allows its own line; above code
//!    (the comment block in between continues the reason) it allows the next
//!    code line, or the whole of a function, method or closure-binding `let`
//!    that starts there, except the items nested in its body. Never a `mod`,
//!    an `impl` or `trait` block, another item or an inner-attribute (file
//!    header) scope. The reason says why the value is not a response member
//!    (a snapshot file, the command's own manifest, a value the slice built)
//!    or why the member is required and every other shape is rejected;
//! 5. it parses: a file `syn` cannot parse is a failure, never a pass;
//! 6. it defines no `macro_rules!` and invokes no macro defined in `src`
//!    unless that definition carries a marker (#4862): the scanner never
//!    expands a macro, so a local one, even one named like a standard macro
//!    (`vec`), is never given the standard grammar.
//!
//! Every finding is counted at its own position; the report groups the
//! findings of a line only to print them, and a `Pending` count is a count of
//! occurrences (#4864).
//!
//! **Threat model.** The scanner guards honest code against an accidental
//! raw read of a recorded response. It has no type inference and expands no
//! macro, so deliberate obfuscation is out of its scope: review and the
//! adoption of the shared module catch that, and evading the scanner (or an
//! inventory entry that misstates what a module reads) is a review blocker.
//! A further evasion blocks a review only when it falls in a class above.
//!
//! Test code (`tests.rs`, `tests/`, `#[cfg(test)]` items, and every file that
//! only test-only module declarations load, such as the file of an
//! out-of-line `#[cfg(test)] mod NAME;` and its submodules, whatever its name,
//! #4869, #4872) is out of scope. A declaration counts at any depth, in a
//! function or constant block too (#4880). The README section "Slices without a live
//! AWS client" is the guide for slice authors.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};

use proc_macro2::{Delimiter, Span, TokenStream, TokenTree};
use syn::ext::IdentExt;
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::visit::{self, Visit};
use syn::{Attribute, Expr, ImplItem, Item, Macro, Pat, Token, TraitItem, Type, UseTree};

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
/// Words of a type, trait or constant name that make it a recording signal.
const REPLAY_WORDS: [&str; 6] = [
    "Recorded",
    "Recording",
    "Replay",
    "RECORDED",
    "RECORDING",
    "REPLAY",
];
/// The crate roots: `lib.rs` holds the command table that names every
/// slice's flags, so it carries no signal of its own.
const ROOTS: [&str; 2] = ["lib", "main"];
/// A marker reason needs at least this many distinct words (#4773).
const MIN_REASON_WORDS: usize = 3;
/// Words that never count towards a reason: a marker whose reason is only
/// these (`todo`, `<reason>`, `fix later`, `this is fine, see above`) is
/// refused. Placeholders, then filler that names no value or source.
const PLACEHOLDER_WORDS: &[&str] = &[
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
    "this",
    "that",
    "is",
    "it",
    "the",
    "an",
    "and",
    "see",
    "above",
    "below",
    "here",
    "fine",
    "safe",
    "okay",
    "trust",
    "me",
    "known",
    "intentional",
    "needed",
    "exempt",
    "exception",
    "marker",
    "code",
];

/// Keywords: their raw spelling (`r#fn`) is a plain name, never the keyword,
/// so the tokenizer keeps it raw instead of normalising it (#4861).
const RUST_KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub",
    "ref", "return", "static", "struct", "super", "trait", "true", "type", "unsafe", "use",
    "where", "while",
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
            let mut start = i;
            while i < chars.len() && is_ident_char(chars[i]) {
                i += 1;
            }
            // A raw identifier `r#name` is the name `name` (#4861); a raw
            // keyword (`r#fn`) stays one token, so it never reads as the
            // keyword.
            let raw = i - start == 1
                && c == 'r'
                && chars.get(i) == Some(&'#')
                && chars
                    .get(i + 1)
                    .is_some_and(|&n| n.is_alphabetic() || n == '_');
            if raw {
                let name_start = i + 1;
                let mut end = name_start;
                while end < chars.len() && is_ident_char(chars[end]) {
                    end += 1;
                }
                let name: String = chars[name_start..end].iter().collect();
                if !RUST_KEYWORDS.contains(&name.as_str()) {
                    start = name_start;
                }
                i = end;
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

/// The lines of the items nested in the body of the leaf item `first..=end`
/// (#4773): a function, `mod`, `impl`, `trait`, `const`, `static` or
/// `macro_rules!` inside a marked function or closure is not covered by its
/// marker, so a function cannot carry a module's worth of readers.
fn nested_item_lines(toks: &[Tok], first: usize, end: usize) -> BTreeSet<usize> {
    let mut lines = BTreeSet::new();
    let mut depth = 0i32;
    let mut open = None;
    for (index, tok) in toks.iter().enumerate().take(end + 1).skip(first) {
        if tok.is("{") && depth == 0 {
            open = Some(index);
            break;
        }
        if tok.is("(") || tok.is("[") || tok.is("{") {
            depth += 1;
        } else if tok.is(")") || tok.is("]") || tok.is("}") {
            depth -= 1;
        }
    }
    let Some(open) = open else {
        return lines;
    };
    let next_is_ident = |i: usize| toks.get(i + 1).is_some_and(Tok::is_ident);
    for i in open + 1..end {
        let tok = &toks[i];
        let item_position = toks[i - 1].is(";")
            || toks[i - 1].is("{")
            || toks[i - 1].is("}")
            || toks[i - 1].is("]")
            || toks[i - 1].is("unsafe");
        let named_value = (tok.is("const") || tok.is("static"))
            && (toks.get(i + 2).is_some_and(|t| t.is(":"))
                || toks.get(i + 1).is_some_and(|t| t.is("mut")));
        let starts_item = (tok.is("fn") && next_is_ident(i))
            || ((tok.is("mod") || tok.is("trait")) && next_is_ident(i))
            || (tok.is("impl") && item_position)
            || (tok.is("macro_rules") && toks.get(i + 1).is_some_and(|t| t.is("!")))
            || named_value;
        if starts_item {
            let item_last = item_end(toks, i).min(end);
            lines.extend(tok.line..=toks[item_last].line);
        }
    }
    lines
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
    /// The syntax tree, or why the file does not parse (a finding in a
    /// recorded slice: an unparsable file is never accepted).
    ast: Result<syn::File, String>,
}

fn parse(file: &SourceFile) -> Parsed {
    let mut lexed = lex(&file.text);
    let toks = drop_test_items(tokenize(&mut lexed));
    let ast = syn::parse_file(&file.text)
        .map_err(|error| format!("line {}: {error}", error.span().start().line));
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
        ast,
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
                let nested = nested_item_lines(&file.toks, first, end);
                markers
                    .allowed
                    .extend((target..=file.toks[end].line).filter(|line| !nested.contains(line)));
            }
        }
    }
    markers
}

// ------------------------------------------------------------- structure
//
// The read rules work on the syntax tree (`syn`), not on token heuristics
// (#4751): a pattern is a pattern wherever the grammar puts it (`match`,
// `if let`, `while let`, `let .. else`, an annotated `let`, a guard, a field
// of a struct pattern, a brace-form variant, behind `&`, `ref`, `@`, `Some` or
// a tuple), and a construction is an expression, never a pattern. Macro
// bodies are opaque to the parser: `matches!` and the standard expression
// macros are parsed with their grammar, and every other macro body (a local
// `macro_rules!`, its invocations, a pattern macro) is scanned token by token
// and fails closed.

/// The 0-based line a span starts on.
fn line_of(span: Span) -> usize {
    span.start().line.saturating_sub(1)
}

/// Macros whose body is `value, PATTERN [if guard] [, message..]`.
const MATCHES_MACROS: [&str; 3] = ["matches", "assert_matches", "debug_assert_matches"];
/// Standard macros whose body is a comma-separated list of expressions (or
/// `value; count`). Any other macro body is scanned token by token.
const EXPR_MACROS: [&str; 21] = [
    "assert",
    "assert_eq",
    "assert_ne",
    "debug_assert",
    "debug_assert_eq",
    "debug_assert_ne",
    "dbg",
    "eprint",
    "eprintln",
    "format",
    "format_args",
    "panic",
    "print",
    "println",
    "todo",
    "unimplemented",
    "unreachable",
    "vec",
    "write",
    "writeln",
    "concat",
];

/// The body of `matches!(value, PATTERN if guard)` and of
/// `assert_matches!(value, PATTERN, message..)`.
struct MatchesArgs {
    value: Expr,
    pattern: Pat,
    guard: Option<Expr>,
    rest: TokenStream,
}

impl Parse for MatchesArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let value = input.parse()?;
        input.parse::<Token![,]>()?;
        let pattern = Pat::parse_multi_with_leading_vert(input)?;
        let guard = if input.peek(Token![if]) {
            input.parse::<Token![if]>()?;
            Some(input.parse()?)
        } else {
            None
        };
        Ok(Self {
            value,
            pattern,
            guard,
            rest: input.parse()?,
        })
    }
}

/// The body of `vec![value; count]`.
struct Repeat {
    value: Expr,
    count: Expr,
}

impl Parse for Repeat {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let value = input.parse()?;
        input.parse::<Token![;]>()?;
        Ok(Self {
            value,
            count: input.parse()?,
        })
    }
}

fn is_cfg_test(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("cfg")
            && attr
                .parse_args::<syn::Ident>()
                .is_ok_and(|ident| ident == "test")
    })
}

fn item_attrs(item: &Item) -> &[Attribute] {
    match item {
        Item::Const(item) => &item.attrs,
        Item::Enum(item) => &item.attrs,
        Item::ExternCrate(item) => &item.attrs,
        Item::Fn(item) => &item.attrs,
        Item::ForeignMod(item) => &item.attrs,
        Item::Impl(item) => &item.attrs,
        Item::Macro(item) => &item.attrs,
        Item::Mod(item) => &item.attrs,
        Item::Static(item) => &item.attrs,
        Item::Struct(item) => &item.attrs,
        Item::Trait(item) => &item.attrs,
        Item::TraitAlias(item) => &item.attrs,
        Item::Type(item) => &item.attrs,
        Item::Union(item) => &item.attrs,
        Item::Use(item) => &item.attrs,
        _ => &[],
    }
}

fn impl_item_attrs(item: &ImplItem) -> &[Attribute] {
    match item {
        ImplItem::Const(item) => &item.attrs,
        ImplItem::Fn(item) => &item.attrs,
        ImplItem::Type(item) => &item.attrs,
        ImplItem::Macro(item) => &item.attrs,
        _ => &[],
    }
}

fn trait_item_attrs(item: &TraitItem) -> &[Attribute] {
    match item {
        TraitItem::Const(item) => &item.attrs,
        TraitItem::Fn(item) => &item.attrs,
        TraitItem::Type(item) => &item.attrs,
        TraitItem::Macro(item) => &item.attrs,
        _ => &[],
    }
}

/// An identifier as the compiler sees it: `r#Array` is `Array` (#4861). Every
/// comparison of a name (a type, a variant, a method, an alias, a macro) goes
/// through this.
fn name_of(ident: &syn::Ident) -> String {
    ident.unraw().to_string()
}

/// Where a finding is: `(0-based line, column)`. Each read is its own finding
/// (#4864); reports group them by line only to print them.
type Pos = (usize, usize);

fn pos_of(span: Span) -> Pos {
    let start = span.start();
    (start.line.saturating_sub(1), start.column)
}

/// Whether `mac` is `macro_rules!` (a definition, not an invocation).
fn is_macro_rules(mac: &Macro) -> bool {
    mac.path.segments.len() == 1 && name_of(&mac.path.segments[0].ident) == "macro_rules"
}

/// The `macro_rules!` items of a parsed file outside test code, as `(name,
/// 0-based line of the macro_rules path)`.
fn macro_definitions(ast: &syn::File) -> Vec<(String, usize)> {
    #[derive(Default)]
    struct Definitions(Vec<(String, usize)>);
    impl<'ast> Visit<'ast> for Definitions {
        fn visit_item(&mut self, item: &'ast Item) {
            if !is_cfg_test(item_attrs(item)) {
                visit::visit_item(self, item);
            }
        }
        fn visit_item_macro(&mut self, item: &'ast syn::ItemMacro) {
            if let (true, Some(ident)) = (is_macro_rules(&item.mac), &item.ident) {
                let line = line_of(item.mac.path.segments[0].ident.span());
                self.0.push((name_of(ident), line));
            }
            visit::visit_item_macro(self, item);
        }
    }
    let mut definitions = Definitions::default();
    definitions.visit_file(ast);
    definitions.0
}

/// The names that, crate-wide, resolve the scan (#4862, #4863, #4775).
#[derive(Default)]
struct Names {
    /// The JSON value type: `Json`, `Value` and their aliases
    /// ([`crate_names`]).
    types: BTreeSet<String>,
    /// `Array`, `Object` and every name a `use` anywhere in `src` gives one
    /// (`use toolkit_core::Json::Array as List`, a re-export, a rename of a
    /// rename): a pattern whose path ends in one is a read in any file, with
    /// any number of segments (#4863).
    variants: BTreeSet<String>,
    /// Every `macro_rules!` name defined anywhere in `src` (#4862). Its
    /// invocations are never parsed as a standard macro, even when it shadows
    /// one (`vec`).
    local_macros: BTreeSet<String>,
    /// The local macros whose every definition carries an accepted
    /// `// recorded-shape:` marker: invoking them is allowed.
    marked_macros: BTreeSet<String>,
}

/// What the structure of one file shows, each finding at its own position
/// (#4864): the shape reads, the aliases of the JSON value type and the local
/// macros it defines or invokes.
#[derive(Default)]
struct Structure {
    reads: BTreeSet<Pos>,
    /// `(position, alias)` for each `type A = path::Json;` and each `Json as
    /// A` in a `use` (#4775).
    aliases: Vec<(Pos, String)>,
    /// Each `macro_rules!` definition and each invocation of a local macro
    /// without a marked definition (#4862).
    macros: BTreeSet<Pos>,
    /// `(original, rename)` for each `use .. as ..` of the file.
    renames: Vec<(String, String)>,
}

struct ShapeScan<'s> {
    names: &'s Names,
    found: Structure,
}

impl ShapeScan<'_> {
    fn is_type(&self, ident: &syn::Ident) -> bool {
        self.names.types.contains(&name_of(ident))
    }

    /// A pattern path: any path ending in `Array` or `Object` (`Json::Array`,
    /// an alias `J::Array`, `Self::Object`, a bare imported `Array`), or in a
    /// name that renames one anywhere in `src` (`variants::List`, a bare
    /// `List`). No type inference: every such pattern counts, whatever the
    /// type it names.
    fn pattern_path(&mut self, path: &syn::Path) {
        let Some(last) = path.segments.last() else {
            return;
        };
        if self.names.variants.contains(&name_of(&last.ident)) {
            self.found.reads.insert(pos_of(last.ident.span()));
        }
    }

    /// A macro body the parser cannot see into: every shape variant with a
    /// payload, every shape method and every renamed variant counts, a
    /// construction included (fail closed).
    fn opaque_tokens(&mut self, tokens: TokenStream) {
        let trees: Vec<TokenTree> = tokens.into_iter().collect();
        for (index, tree) in trees.iter().enumerate() {
            match tree {
                TokenTree::Group(group) => self.opaque_tokens(group.stream()),
                TokenTree::Ident(ident) => {
                    let name = name_of(ident);
                    let payload = matches!(
                        trees.get(index + 1),
                        Some(TokenTree::Group(group)) if group.delimiter() != Delimiter::Bracket
                    );
                    let variant = self.names.variants.contains(&name);
                    if (variant && payload) || SHAPE_METHODS.contains(&name.as_str()) {
                        self.found.reads.insert(pos_of(ident.span()));
                    }
                }
                _ => {}
            }
        }
    }

    /// A `use` tree under `parent`: importing a shape variant of the JSON
    /// type (`use Json::{Array, Object}`, `use Json::*`) is a read, and
    /// renaming the JSON type (`Json as J`, `Json::{self as J}`) an alias.
    fn use_tree(&mut self, tree: &UseTree, parent: Option<&syn::Ident>) {
        let parent_is_type = parent.is_some_and(|parent| self.is_type(parent));
        match tree {
            UseTree::Path(path) => self.use_tree(&path.tree, Some(&path.ident)),
            UseTree::Group(group) => {
                for item in &group.items {
                    self.use_tree(item, parent);
                }
            }
            UseTree::Glob(glob) => {
                if parent_is_type {
                    self.found.reads.insert(pos_of(glob.star_token.span));
                }
            }
            UseTree::Name(name) => {
                let variant = SHAPE_VARIANTS.contains(&name_of(&name.ident).as_str());
                if parent_is_type && variant {
                    self.found.reads.insert(pos_of(name.ident.span()));
                }
            }
            UseTree::Rename(rename) => {
                let original = name_of(&rename.ident);
                let renamed = name_of(&rename.rename);
                self.found.renames.push((original.clone(), renamed.clone()));
                if parent_is_type && SHAPE_VARIANTS.contains(&original.as_str()) {
                    self.found.reads.insert(pos_of(rename.ident.span()));
                }
                let names_type = if original == "self" {
                    parent_is_type
                } else {
                    self.is_type(&rename.ident)
                };
                if names_type && renamed != "_" {
                    self.found
                        .aliases
                        .push((pos_of(rename.rename.span()), renamed));
                }
            }
        }
    }
}

impl<'ast> Visit<'ast> for ShapeScan<'_> {
    fn visit_item(&mut self, item: &'ast Item) {
        if !is_cfg_test(item_attrs(item)) {
            visit::visit_item(self, item);
        }
    }

    fn visit_impl_item(&mut self, item: &'ast ImplItem) {
        if !is_cfg_test(impl_item_attrs(item)) {
            visit::visit_impl_item(self, item);
        }
    }

    fn visit_trait_item(&mut self, item: &'ast TraitItem) {
        if !is_cfg_test(trait_item_attrs(item)) {
            visit::visit_trait_item(self, item);
        }
    }

    fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
        self.use_tree(&item.tree, None);
    }

    fn visit_item_type(&mut self, item: &'ast syn::ItemType) {
        let mut ty = &*item.ty;
        loop {
            match ty {
                Type::Paren(inner) => ty = &inner.elem,
                Type::Group(inner) => ty = &inner.elem,
                _ => break,
            }
        }
        if let Type::Path(path) = ty {
            let names_type = path
                .path
                .segments
                .last()
                .is_some_and(|last| self.is_type(&last.ident));
            if path.qself.is_none() && names_type {
                self.found
                    .aliases
                    .push((pos_of(item.type_token.span), name_of(&item.ident)));
            }
        }
        visit::visit_item_type(self, item);
    }

    fn visit_item_macro(&mut self, item: &'ast syn::ItemMacro) {
        // A local `macro_rules!` in a recorded slice needs a marker (#4862):
        // its expansion is never analysed.
        if is_macro_rules(&item.mac) && item.ident.is_some() {
            self.found
                .macros
                .insert(pos_of(item.mac.path.segments[0].ident.span()));
        }
        visit::visit_item_macro(self, item);
    }

    fn visit_pat(&mut self, pat: &'ast Pat) {
        match pat {
            Pat::TupleStruct(pattern) => self.pattern_path(&pattern.path),
            Pat::Struct(pattern) => self.pattern_path(&pattern.path),
            Pat::Path(pattern) => self.pattern_path(&pattern.path),
            // A pattern a macro builds is not visible here: refused.
            Pat::Macro(pattern) => {
                self.found.reads.insert(pos_of(pattern.mac.bang_token.span));
            }
            _ => {}
        }
        visit::visit_pat(self, pat);
    }

    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        if SHAPE_METHODS.contains(&name_of(&call.method).as_str()) {
            self.found.reads.insert(pos_of(call.method.span()));
        }
        visit::visit_expr_method_call(self, call);
    }

    fn visit_path(&mut self, path: &'ast syn::Path) {
        // `Value::as_array` as a function value.
        if let Some(last) = path.segments.last() {
            if SHAPE_METHODS.contains(&name_of(&last.ident).as_str()) {
                self.found.reads.insert(pos_of(last.ident.span()));
            }
        }
        visit::visit_path(self, path);
    }

    fn visit_macro(&mut self, mac: &'ast Macro) {
        let Some(segment) = mac.path.segments.last() else {
            return;
        };
        let name = name_of(&segment.ident);
        if self.names.local_macros.contains(&name) {
            // A local macro, even one that shadows a standard name (`vec`):
            // its body is opaque and its use needs a marked definition.
            if !self.names.marked_macros.contains(&name) {
                self.found.macros.insert(pos_of(segment.ident.span()));
            }
            self.opaque_tokens(mac.tokens.clone());
            return;
        }
        // The standard macros, by their plain or `std::`/`core::`/`alloc::`
        // path.
        let standard = mac.path.segments.len() == 1
            || (mac.path.segments.len() == 2
                && ["std", "core", "alloc"]
                    .contains(&name_of(&mac.path.segments[0].ident).as_str()));
        if !standard {
            self.opaque_tokens(mac.tokens.clone());
            return;
        }
        if MATCHES_MACROS.contains(&name.as_str()) {
            if let Ok(args) = mac.parse_body::<MatchesArgs>() {
                self.visit_expr(&args.value);
                self.visit_pat(&args.pattern);
                if let Some(guard) = &args.guard {
                    self.visit_expr(guard);
                }
                self.opaque_tokens(args.rest);
                return;
            }
        } else if EXPR_MACROS.contains(&name.as_str()) {
            let list = Punctuated::<Expr, Token![,]>::parse_terminated;
            if let Ok(values) = mac.parse_body_with(list) {
                for value in &values {
                    self.visit_expr(value);
                }
                return;
            }
            if let Ok(repeat) = mac.parse_body::<Repeat>() {
                self.visit_expr(&repeat.value);
                self.visit_expr(&repeat.count);
                return;
            }
        }
        self.opaque_tokens(mac.tokens.clone());
    }
}

/// What the scan finds in a parsed file under `names`.
fn structure(ast: &syn::File, names: &Names) -> Structure {
    let mut scan = ShapeScan {
        names,
        found: Structure::default(),
    };
    scan.visit_file(ast);
    scan.found
}

/// The crate-wide names of `files` ([`Names`]). The JSON type names and the
/// variant names are followed to a fixed point: an alias of an alias, a rename
/// of a renamed variant, a re-export in another module or file (#4775,
/// #4863).
fn crate_names(files: &[Parsed]) -> Names {
    let mut names = Names {
        types: SHAPE_TYPES.iter().map(ToString::to_string).collect(),
        variants: SHAPE_VARIANTS.iter().map(ToString::to_string).collect(),
        ..Names::default()
    };
    let parsed = || {
        files
            .iter()
            .filter_map(|file| file.ast.as_ref().ok().map(|ast| (file, ast)))
    };
    loop {
        let before = (names.types.len(), names.variants.len());
        for (_, ast) in parsed() {
            let found = structure(ast, &names);
            for (_, alias) in found.aliases {
                names.types.insert(alias);
            }
            for (original, rename) in found.renames {
                if names.variants.contains(&original) && rename != "_" {
                    names.variants.insert(rename);
                }
            }
        }
        if (names.types.len(), names.variants.len()) == before {
            break;
        }
    }
    // A macro is marked only when every definition of its name is.
    let mut unmarked = BTreeSet::new();
    for (file, ast) in parsed() {
        let allowed = markers(file).allowed;
        for (name, line) in macro_definitions(ast) {
            if !allowed.contains(&line) {
                unmarked.insert(name.clone());
            }
            names.local_macros.insert(name);
        }
    }
    names.marked_macros = names.local_macros.difference(&unmarked).cloned().collect();
    names
}

/// What the read rules find in one file of a recorded slice, each entry
/// `file:line: source` (1-based line). The entries group the findings of a
/// line for the reader; `occurrences` counts each one (#4864).
#[derive(Default)]
struct FileFindings {
    /// Shape reads no marker allows.
    reads: Vec<String>,
    /// Aliases of the JSON value type no marker allows (#4775).
    aliases: Vec<String>,
    /// Local macro definitions and invocations no marker allows (#4862).
    macros: Vec<String>,
    /// Markers refused for a blank or placeholder reason (#4773).
    markers: Vec<String>,
    /// `file: why` when the file does not parse.
    unparsed: Vec<String>,
    /// Every finding counted at its own position: two reads on one line are
    /// two (#4864).
    occurrences: usize,
}

fn file_findings(file: &Parsed, names: &Names) -> FileFindings {
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
    let mut occurrences = 0;
    let mut unmarked = |positions: Vec<Pos>| -> Vec<String> {
        let mut per_line: BTreeMap<usize, usize> = BTreeMap::new();
        for (line, _) in positions {
            if !markers.allowed.contains(&line) {
                *per_line.entry(line).or_default() += 1;
                occurrences += 1;
            }
        }
        per_line
            .into_iter()
            .map(|(line, count)| match count {
                1 => entry(line),
                _ => format!("{} [{count} on this line]", entry(line)),
            })
            .collect()
    };
    let found = match &file.ast {
        Ok(ast) => structure(ast, names),
        Err(why) => {
            return FileFindings {
                unparsed: vec![format!("{}: does not parse: {why}", file.relative)],
                occurrences: 1,
                ..FileFindings::default()
            }
        }
    };
    let reads = unmarked(found.reads.into_iter().collect());
    let aliases = unmarked(found.aliases.into_iter().map(|(pos, _)| pos).collect());
    let macros = unmarked(found.macros.into_iter().collect());
    let markers: Vec<String> = markers.refused.iter().copied().map(entry).collect();
    FileFindings {
        reads,
        aliases,
        macros,
        occurrences: occurrences + markers.len(),
        markers,
        unparsed: Vec::new(),
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

/// A type, trait, variant or constant name that mentions a recording
/// (`RecordedClient`, `RecordedPriceList`, `ReplayQueue`, `MAX_RECORDED_BYTES`).
/// A function or variable name (`recorded`, `load_recording`) is not one.
fn is_replay_name(name: &str) -> bool {
    name.starts_with(|c: char| c.is_ascii_uppercase())
        && REPLAY_WORDS.iter().any(|word| name.contains(word))
}

/// Whether `file` mentions a recorded replay type, trait or constant.
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

/// The slices whose `CommandSpec` row names a recording flag, inline or
/// through a constant. Since #4576 a slice registers its rows in its own
/// module, so a row there counts for that slice; a row of `lib.rs` still counts
/// for the slice its handler routes to.
fn command_slices(files: &[Parsed], constants: &BTreeSet<String>) -> BTreeSet<String> {
    let known: BTreeSet<&str> = files.iter().map(|file| file.slice.as_str()).collect();
    let mut found = BTreeSet::new();
    for file in files {
        let toks = &file.toks;
        for (i, tok) in toks.iter().enumerate() {
            let is_row = tok.is("CommandSpec")
                && toks.get(i + 1).is_some_and(|t| t.is("{"))
                && i > 0
                && (toks[i - 1].is("[") || toks[i - 1].is(",") || toks[i - 1].is("::"));
            if !is_row {
                continue;
            }
            let row = &toks[i + 1..=matching(toks, i + 1)];
            let names_flag = row
                .iter()
                .any(|t| is_recording_flag(t) || (t.is_ident() && constants.contains(&t.text)));
            if !names_flag {
                continue;
            }
            if file.relative != "lib.rs" {
                found.insert(file.slice.clone());
                continue;
            }
            let handler = row
                .windows(2)
                .position(|pair| pair[0].is("handler") && pair[1].is(":"));
            let Some(handler) = handler else { continue };
            let slice = row[handler + 2..]
                .iter()
                .take_while(|t| !t.is(","))
                .find(|t| t.is_ident() && known.contains(t.text.as_str()));
            if let Some(slice) = slice {
                found.insert(slice.text.clone());
            }
        }
    }
    found
}

/// How the inventory classifies a top-level slice (#4752). The inventory
/// lists every slice of `src`: a new slice fails the contract until its author
/// classifies it, whatever its client, flag or file layout is called.
#[derive(Clone, Copy, Debug)]
enum Class {
    /// Replays recorded AWS calls: it must import the shared module and every
    /// read rule applies, with or without a detected signal.
    Recorded,
    /// Reads recorded AWS responses by hand and has not moved onto the shared
    /// module yet; `issue` tracks the move. Its findings are counted and must
    /// equal `findings`, so a new hand-written read fails and a removed one
    /// lowers the count in the same change.
    Pending {
        issue: &'static str,
        findings: usize,
    },
    /// Neither replays nor reads a recording (the reason says what it reads
    /// instead). Any recording signal in it fails the contract.
    NotRecorded(&'static str),
}

/// What the contract found in a set of source files.
#[derive(Debug, Default)]
struct Report {
    /// Recorded slice -> the signals that put it in scope.
    scope: BTreeMap<String, BTreeSet<&'static str>>,
    /// Slices with a flag, command, type or inventory signal that never
    /// import the module.
    unadopted: Vec<String>,
    /// `file: helper` for a shared helper defined in a recorded slice.
    helpers: Vec<String>,
    /// `file:line: source` for an unmarked shape read in a recorded slice.
    reads: Vec<String>,
    /// `file:line: source` for an unmarked alias of the JSON value type in a
    /// recorded slice (#4775).
    aliases: Vec<String>,
    /// `file:line: source` for an unmarked `macro_rules!` definition, or an
    /// invocation of a local macro whose definition is unmarked, in a
    /// recorded slice (#4862).
    macros: Vec<String>,
    /// `file:line: source` for a marker with a blank or placeholder reason in
    /// a recorded slice (#4773).
    markers: Vec<String>,
    /// `file: why` for a file of a recorded slice that does not parse.
    unparsed: Vec<String>,
    /// Inventory failures (#4752): an unclassified slice, a stale entry, a
    /// not-recorded slice with a recording signal, a pending count that moved.
    inventory: Vec<String>,
}

impl Report {
    fn is_clean(&self) -> bool {
        self.unadopted.is_empty()
            && self.helpers.is_empty()
            && self.reads.is_empty()
            && self.aliases.is_empty()
            && self.macros.is_empty()
            && self.markers.is_empty()
            && self.unparsed.is_empty()
            && self.inventory.is_empty()
    }
}

/// Applies `inventory` to the signal scope of `report` over the slices
/// `present` (#4752): records every inventory failure, puts each recorded
/// slice in scope and takes each pending slice out of it. Returns the pending
/// slices with their Issue and pinned finding count.
fn classify(
    report: &mut Report,
    present: &BTreeSet<&str>,
    inventory: &[(&str, Class)],
) -> Vec<(String, &'static str, usize)> {
    let mut pending = Vec::new();
    for slice in present {
        let listed = inventory.iter().any(|(name, _)| name == slice);
        if !listed && !ROOTS.contains(slice) {
            report.inventory.push(format!(
                "unclassified slice {slice}: add it to the inventory as recorded, pending or \
                 not recorded"
            ));
        }
    }
    for (slice, class) in inventory {
        if !present.contains(slice) {
            report
                .inventory
                .push(format!("stale inventory entry {slice}: no such slice"));
            continue;
        }
        match class {
            Class::Recorded => {
                report
                    .scope
                    .entry((*slice).to_string())
                    .or_default()
                    .insert("inventory");
            }
            Class::Pending { issue, findings } => {
                report.scope.remove(*slice);
                pending.push(((*slice).to_string(), *issue, *findings));
            }
            Class::NotRecorded(reason) => {
                if !is_specific_reason(reason) {
                    report.inventory.push(format!(
                        "{slice} is classified not recorded without a specific reason: \
                         {reason:?}"
                    ));
                }
                if let Some(signals) = report.scope.get(*slice) {
                    report.inventory.push(format!(
                        "{slice} is classified not recorded but shows recording signals \
                         {signals:?}"
                    ));
                }
            }
        }
    }
    pending
}

/// Where a module file's own declarations resolve (#4872): `mod NAME;` loads
/// `default/NAME.rs` or `default/NAME/mod.rs`, `#[path = "P"] mod NAME;` loads
/// `paths/P`. A mod-rs file (`a/mod.rs`, `lib.rs`, `main.rs`, any file loaded
/// through `#[path]`) has both in its directory; a non-mod-rs `a/b.rs` has
/// `default` `a/b` and `paths` `a`.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ModDirs {
    default: String,
    paths: String,
}

impl ModDirs {
    fn owned(dir: String) -> Self {
        Self {
            default: dir.clone(),
            paths: dir,
        }
    }
}

/// One module declaration that loads a file: who declares it, the file, and
/// whether the declaration is test-only (`#[cfg(test)]` on it or on an
/// enclosing inline module).
struct ModLoad {
    declarer: String,
    target: String,
    test: bool,
    /// The directory a `#[path]`-loaded file owns, as a mod-rs file.
    owns: Option<String>,
}

/// `rest` (a `/`-separated relative path) resolved against `dir`.
fn join_path(dir: &str, rest: &str) -> String {
    let mut parts: Vec<&str> = dir.split('/').filter(|part| !part.is_empty()).collect();
    for part in rest.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    parts.join("/")
}

/// The `#[path = "P"]` of a module declaration.
fn path_attr(attrs: &[Attribute]) -> Option<String> {
    attrs.iter().find_map(|attr| match &attr.meta {
        syn::Meta::NameValue(meta) if meta.path.is_ident("path") => match &meta.value {
            Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(path),
                ..
            }) => Some(path.value()),
            _ => None,
        },
        _ => None,
    })
}

/// The directories of `relative` when its parent loads it by the default
/// path: the compiler's mod-rs / non-mod-rs rule.
fn natural_dirs(relative: &str) -> ModDirs {
    let stem = relative.strip_suffix(".rs").unwrap_or(relative);
    match stem.rsplit_once('/') {
        Some((parent, "mod")) => ModDirs::owned(parent.to_string()),
        None if ROOTS.contains(&stem) || stem == "mod" => ModDirs::owned(String::new()),
        Some((parent, _)) => ModDirs {
            default: stem.to_string(),
            paths: parent.to_string(),
        },
        None => ModDirs {
            default: stem.to_string(),
            paths: String::new(),
        },
    }
}

/// Every module declaration of the file `declarer` that loads a file of
/// `present`, at any depth (#4880): the file's items, inline modules, and the
/// blocks of functions, methods, constants, statics and expressions. A
/// declaration inherits `#[cfg(test)]` from any enclosing item. Whatever the
/// walk cannot classify counts as a production load, which keeps a file
/// scanned: a default-path `mod NAME;` in a block (rustc refuses it), and a
/// `mod NAME` in a macro's or unparsed item's tokens.
struct ModuleLoads<'a> {
    declarer: &'a str,
    present: &'a BTreeSet<&'a str>,
    dirs: ModDirs,
    /// Inside a `#[cfg(test)]` item.
    test: bool,
    /// Inside a block, where rustc resolves against the module's directory
    /// alone and refuses a default-path `mod NAME;`.
    in_block: bool,
    out: &'a mut Vec<ModLoad>,
}

impl ModuleLoads<'_> {
    fn load(&mut self, target: String, test: bool, owns: Option<String>) {
        if self.present.contains(target.as_str()) {
            self.out.push(ModLoad {
                declarer: self.declarer.to_string(),
                target,
                test,
                owns,
            });
        }
    }

    /// `#[path = "P"] mod NAME;` loads `P`. A `#[path]` file is a mod-rs
    /// file: its children live beside it.
    fn path_load(&mut self, path: &str, test: bool) {
        let target = join_path(&self.dirs.paths, path);
        let owns = target
            .rsplit_once('/')
            .map_or(String::new(), |(parent, _)| parent.to_string());
        self.load(target, test, Some(owns));
    }

    /// `mod NAME;` loads `NAME.rs` or `NAME/mod.rs`.
    fn default_load(&mut self, name: &str, test: bool) {
        for file in [format!("{name}.rs"), format!("{name}/mod.rs")] {
            let target = join_path(&self.dirs.default, &file);
            self.load(target, test, None);
        }
    }

    /// `walk` with the context swapped in; the current one comes back after.
    fn within(&mut self, dirs: ModDirs, test: bool, in_block: bool, walk: impl FnOnce(&mut Self)) {
        let dirs = std::mem::replace(&mut self.dirs, dirs);
        let test = std::mem::replace(&mut self.test, test);
        let in_block = std::mem::replace(&mut self.in_block, in_block);
        walk(self);
        self.dirs = dirs;
        self.test = test;
        self.in_block = in_block;
    }

    /// Module declarations hidden in tokens `syn` does not parse: when a
    /// `mod NAME` is among them, each such `NAME` loads its default files and
    /// each string literal its `#[path]` file, all as production loads.
    fn token_loads(&mut self, tokens: &TokenStream) {
        fn collect(tokens: &TokenStream, names: &mut Vec<String>, paths: &mut Vec<String>) {
            let mut after_mod = false;
            for token in tokens.clone() {
                match &token {
                    TokenTree::Ident(ident) if after_mod => names.push(name_of(ident)),
                    TokenTree::Group(group) => collect(&group.stream(), names, paths),
                    TokenTree::Literal(_) => {
                        if let Ok(path) = syn::parse2::<syn::LitStr>(token.clone().into()) {
                            paths.push(path.value());
                        }
                    }
                    _ => {}
                }
                after_mod = matches!(&token, TokenTree::Ident(ident) if ident == "mod");
            }
        }
        let (mut names, mut paths) = (Vec::new(), Vec::new());
        collect(tokens, &mut names, &mut paths);
        if names.is_empty() {
            return;
        }
        for name in &names {
            self.default_load(name, false);
        }
        for path in &paths {
            self.path_load(path, false);
        }
    }

    /// The items of an inline module, walked with its context.
    fn inline(&mut self, items: &[Item], dirs: ModDirs, test: bool, in_block: bool) {
        self.within(dirs, test, in_block, |this| {
            for item in items {
                this.visit_item(item);
            }
        });
    }
}

impl<'ast> Visit<'ast> for ModuleLoads<'_> {
    fn visit_item(&mut self, item: &'ast Item) {
        if let Item::Verbatim(tokens) = item {
            self.token_loads(tokens);
        }
        let test = self.test || is_cfg_test(item_attrs(item));
        let (dirs, in_block) = (self.dirs.clone(), self.in_block);
        self.within(dirs, test, in_block, |this| visit::visit_item(this, item));
    }

    fn visit_impl_item(&mut self, item: &'ast ImplItem) {
        let test = self.test || is_cfg_test(impl_item_attrs(item));
        let (dirs, in_block) = (self.dirs.clone(), self.in_block);
        self.within(dirs, test, in_block, |this| {
            visit::visit_impl_item(this, item);
        });
    }

    fn visit_trait_item(&mut self, item: &'ast TraitItem) {
        let test = self.test || is_cfg_test(trait_item_attrs(item));
        let (dirs, in_block) = (self.dirs.clone(), self.in_block);
        self.within(dirs, test, in_block, |this| {
            visit::visit_trait_item(this, item);
        });
    }

    fn visit_block(&mut self, block: &'ast syn::Block) {
        // In a block rustc resolves against the module's own directory and
        // drops the non-mod-rs offset: `a/b.rs` resolves against `a`.
        let dirs = ModDirs::owned(self.dirs.paths.clone());
        let test = self.test;
        self.within(dirs, test, true, |this| visit::visit_block(this, block));
    }

    fn visit_item_mod(&mut self, module: &'ast syn::ItemMod) {
        let test = self.test || is_cfg_test(&module.attrs);
        let name = name_of(&module.ident);
        match (&module.content, path_attr(&module.attrs)) {
            // An inline module loads no file itself (#4872); its out-of-line
            // children resolve below its `#[path]` directory, or below it.
            (Some((_, items)), Some(path)) => {
                let dirs = ModDirs::owned(join_path(&self.dirs.paths, &path));
                self.inline(items, dirs, test, false);
            }
            (Some((_, items)), None) => {
                let dirs = ModDirs::owned(join_path(&self.dirs.default, &name));
                let in_block = self.in_block;
                self.inline(items, dirs, test, in_block);
            }
            (None, Some(path)) => self.path_load(&path, test),
            // rustc refuses a default-path module in a block: fail safe.
            (None, None) => self.default_load(&name, test && !self.in_block),
        }
    }

    fn visit_macro(&mut self, mac: &'ast Macro) {
        self.token_loads(&mac.tokens);
        visit::visit_macro(self, mac);
    }
}

/// The files (relative to `src`) that only test code loads (#4869, #4872):
/// every module declaration that loads the file is test-only, either
/// `#[cfg(test)]` itself (or inside a `#[cfg(test)]` item or inline module) or
/// in a file that is itself test-only. Declarations count at any depth,
/// function and constant blocks included (#4880). A file no declaration in
/// sight loads, or one that any other declaration loads (`#[cfg(not(test))]`,
/// no `cfg`, another `cfg`, one the walk cannot classify), stays scanned. An inline `#[cfg(test)] mod NAME { .. }` claims no
/// file, and `#[path = "P"]` loads `P`, never the default `NAME.rs`.
fn test_only_files(files: &[Parsed]) -> BTreeSet<String> {
    let present: BTreeSet<&str> = files.iter().map(|file| file.relative.as_str()).collect();
    let asts: BTreeMap<&str, &syn::File> = files
        .iter()
        .filter_map(|file| Some((file.relative.as_str(), file.ast.as_ref().ok()?)))
        .collect();
    // Walk each file by its default-path directories and, once a `#[path]`
    // reaches it, by those of a mod-rs file too.
    let mut queue: VecDeque<(String, ModDirs)> = asts
        .keys()
        .map(|relative| ((*relative).to_string(), natural_dirs(relative)))
        .collect();
    let mut walked = BTreeSet::new();
    let mut loads: Vec<ModLoad> = Vec::new();
    while let Some((relative, dirs)) = queue.pop_front() {
        if !walked.insert((relative.clone(), dirs.clone())) {
            continue;
        }
        let Some(ast) = asts.get(relative.as_str()) else {
            continue;
        };
        let start = loads.len();
        ModuleLoads {
            declarer: &relative,
            present: &present,
            dirs,
            test: false,
            in_block: false,
            out: &mut loads,
        }
        .visit_file(ast);
        for load in &loads[start..] {
            if let Some(owns) = &load.owns {
                queue.push_back((load.target.clone(), ModDirs::owned(owns.clone())));
            }
        }
    }
    // The least fixed point: a file is test-only once every load of it is.
    let mut test_only = BTreeSet::new();
    loop {
        let mut by_target: BTreeMap<&str, bool> = BTreeMap::new();
        for load in &loads {
            let only_test = load.test || test_only.contains(&load.declarer);
            *by_target.entry(load.target.as_str()).or_insert(true) &= only_test;
        }
        let next: BTreeSet<String> = by_target
            .into_iter()
            .filter(|&(_, only_test)| only_test)
            .map(|(target, _)| target.to_string())
            .collect();
        if next == test_only {
            return test_only;
        }
        test_only = next;
    }
}

/// The contract over `files`, scoped by the recording signals alone.
fn analyse(files: &[SourceFile]) -> Report {
    analyse_with(files, None)
}

/// The contract over `files`; with an inventory, every slice must be
/// classified and the classification decides the scope.
fn analyse_with(files: &[SourceFile], inventory: Option<&[(&str, Class)]>) -> Report {
    let mut parsed: Vec<Parsed> = files.iter().map(parse).collect();
    let test_only = test_only_files(&parsed);
    parsed.retain(|file| !test_only.contains(&file.relative));
    let constants = flag_constants(&parsed);
    let names = crate_names(&parsed);
    let mut report = Report::default();
    let add = |scope: &mut BTreeMap<String, BTreeSet<&'static str>>, slice: &str, signal| {
        scope.entry(slice.to_string()).or_default().insert(signal);
    };
    for file in parsed
        .iter()
        .filter(|file| !ROOTS.contains(&file.slice.as_str()))
    {
        if declares_recording_flag(file) {
            add(&mut report.scope, &file.slice, "flag");
        }
        if mentions_replay_type(file) {
            add(&mut report.scope, &file.slice, "type");
        }
        if imports_shared(file) {
            add(&mut report.scope, &file.slice, "import");
        }
    }
    for slice in command_slices(&parsed, &constants) {
        add(&mut report.scope, &slice, "command");
    }
    let pending = match inventory {
        Some(inventory) => {
            let present: BTreeSet<&str> = parsed.iter().map(|file| file.slice.as_str()).collect();
            classify(&mut report, &present, inventory)
        }
        None => Vec::new(),
    };
    let findings_of = |file: &Parsed| {
        let helpers: Vec<String> = redefined_helpers(file)
            .into_iter()
            .map(|helper| format!("{}: fn {helper}", file.relative))
            .collect();
        (helpers, file_findings(file, &names))
    };
    let slices: Vec<String> = report.scope.keys().cloned().collect();
    for slice in slices {
        let in_slice = || parsed.iter().filter(|file| file.slice == slice);
        let signals = &report.scope[&slice];
        let needs_module = signals.iter().any(|signal| *signal != "import");
        if needs_module && !in_slice().any(imports_shared) {
            report.unadopted.push(slice.clone());
        }
        for file in in_slice() {
            let (helpers, findings) = findings_of(file);
            report.helpers.extend(helpers);
            report.reads.extend(findings.reads);
            report.aliases.extend(findings.aliases);
            report.macros.extend(findings.macros);
            report.markers.extend(findings.markers);
            report.unparsed.extend(findings.unparsed);
        }
    }
    // The ratchet counts occurrences, never lines: a read added beside one
    // already counted raises the count (#4864).
    for (slice, issue, expected) in pending {
        let found: usize = parsed
            .iter()
            .filter(|file| file.slice == slice)
            .map(|file| {
                let (helpers, findings) = findings_of(file);
                helpers.len() + findings.occurrences
            })
            .sum();
        if found != expected {
            report.inventory.push(format!(
                "pending slice {slice} ({issue}) has {found} findings, the inventory says \
                 {expected}: move new reads onto the shared accessors, and lower the count when \
                 a read moves"
            ));
        }
    }
    report
}

// ------------------------------------------------------ the real sources

/// Every top-level slice of `src` (#4752). A new slice fails the contract
/// until it is listed here; a slice listed as not recorded fails as soon as
/// it shows a recording signal.
const INVENTORY: &[(&str, Class)] = &[
    (
        "ai_data",
        Class::NotRecorded(
            "dataset samples and metric definitions the user supplies; S3 and SageMaker go \
             through traits wired to unavailable services",
        ),
    ),
    ("ai_models", Class::Recorded),
    (
        "asm_exec",
        Class::NotRecorded("resolves Secrets Manager references through the live SDK client"),
    ),
    (
        "aurora",
        Class::NotRecorded(
            "remote reads go through `api::AwsApi`, wired to `NoAws`; the offline modes take \
             command-line figures",
        ),
    ),
    ("bedrock_agent", Class::Recorded),
    ("bedrock_probes", Class::Recorded),
    (
        "bedrock_report",
        Class::NotRecorded("the skill's own report payloads and phase-result files"),
    ),
    ("bedrock_resolve", Class::Recorded),
    ("cloudwatch", Class::Recorded),
    ("docgraph", Class::Recorded),
    (
        "dynamodb",
        Class::NotRecorded(
            "a pure cost calculator and a design loop over the skill's own state files",
        ),
    ),
    ("dynamodb_bench", Class::Recorded),
    ("dynamodb_deploy", Class::Recorded),
    // Includes the tunnel commands of `elasticache/tunnel/` (#4393, round 19): they
    // replay `--recorded` through `toolkit_aws::recorded`, and their flag constant
    // `TUNNEL_RECORDED_FLAG` is unique in `src` (#4933).
    ("elasticache", Class::Recorded),
    (
        "manifests",
        Class::NotRecorded("the repository's marketplace and plugin manifests"),
    ),
    ("marketplace", Class::Recorded),
    (
        "msk",
        Class::NotRecorded("sizing, compatibility and simulation inputs the user writes"),
    ),
    ("relational", Class::Recorded),
    (
        "registry",
        Class::NotRecorded("the per-slice command routes and help text built at compile time"),
    ),
    (
        "safe_text",
        Class::NotRecorded("escapes untrusted text for diagnostics; reads nothing"),
    ),
    (
        "skills",
        Class::NotRecorded("registers `skills list`, which lists the repository skill directories"),
    ),
    (
        "spec",
        Class::NotRecorded("plugin manifests against the vendored Agent Plugins schemas"),
    ),
    (
        "startup",
        Class::NotRecorded(
            "the startup advisor's repository artifacts, fixtures and model tables; the \
             model-path probe is live and its recording client is test-only",
        ),
    ),
    (
        "sync",
        Class::NotRecorded("synchronizes the vendored skill copies of the repository"),
    ),
    (
        "telemetry",
        Class::NotRecorded("the consent record, migration run files and skill bundles on disk"),
    ),
];

/// The whole contract over the real sources, scoped by [`INVENTORY`].
fn contract() -> Report {
    analyse_with(&sources(), Some(INVENTORY))
}

#[test]
fn every_slice_is_classified_and_the_classification_holds() {
    let report = contract();
    assert!(
        report.inventory.is_empty(),
        "the slice inventory of the recorded contract (#4752) does not hold:\n{}\nA slice that \
         replays or reads recorded AWS calls is `Class::Recorded` and uses `{SHARED_CRATE}::\
         {SHARED_MODULE}`; any other slice is `Class::NotRecorded` with what it reads instead.",
        report.inventory.join("\n")
    );
    assert!(report.unparsed.is_empty(), "{}", report.unparsed.join("\n"));
}

#[test]
fn the_known_recorded_slices_are_in_scope() {
    let report = contract();
    for slice in ["ai_models", "bedrock_agent", "docgraph", "relational"] {
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
    let report = contract();
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
    let report = contract();
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
    let report = contract();
    assert!(
        report.helpers.is_empty(),
        "recorded slices must use the `{SHARED_CRATE}::{SHARED_MODULE}` helpers instead of \
         their own copies (#4727):\n{}",
        report.helpers.join("\n")
    );
}

#[test]
fn recorded_slices_read_members_through_the_shared_helpers() {
    let report = contract();
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
    let report = contract();
    assert!(
        report.aliases.is_empty() && report.markers.is_empty(),
        "recorded slices (#4727) must not alias the JSON value type (#4775):\n{}\n\
         and every `// {MARKER} <reason>` needs a specific reason (#4773):\n{}",
        report.aliases.join("\n"),
        report.markers.join("\n")
    );
}

#[test]
fn recorded_slices_define_and_invoke_no_unmarked_local_macro() {
    let report = contract();
    assert!(
        report.macros.is_empty(),
        "recorded slices (#4727) may not define or invoke a local `macro_rules!` whose \
         definition has no `// {MARKER} <reason>` (#4862): the scanner never expands one\n{}",
        report.macros.join("\n")
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

// ------------------------------- #4751-#4775 hardening on the syntax tree

/// Every evasion of the token scanner of b8204e9d this change closes: each
/// one passed that scanner's whole contract and fails this one. `(finding,
/// label, files)`; the slice is `fresh`, scoped by its import alone.
fn fourth_review_evasions() -> Vec<(&'static str, &'static str, Vec<SourceFile>)> {
    let one = |text: String| vec![file("fresh/deep/recording.rs", &text)];
    let use_line = "use toolkit_aws::recorded::Step;";
    vec![
        (
            "#4751",
            "variant behind & in a struct-field pattern",
            one(format!(
                "{use_line}\nstruct Holder<'a> {{ body: &'a Json }}\nfn n(h: Holder) -> usize {{\n    match h {{ Holder {{ body: &Json::Array(ref v) }} => v.len(), _ => 0 }}\n}}"
            )),
        ),
        (
            "#4751",
            "pattern forwarded through a local macro",
            one(format!(
                "{use_line}\nmacro_rules! is {{ ($v:expr, $p:pat) => {{ matches!($v, $p) }}; }}\nfn listed(r: &Json) -> bool {{\n    is!(r.get(\"Items\"), Some(Json::Array(_)))\n}}"
            )),
        ),
        (
            "#4751",
            "pattern built by a local macro",
            one(format!(
                "{use_line}\nmacro_rules! list {{ ($p:pat) => {{ Json::Array($p) }}; }}\nfn n(r: &Json) -> usize {{\n    match r {{ list!(v) => v.len(), _ => 0 }}\n}}"
            )),
        ),
        (
            "#4752",
            "replay type without Client, Step or Queue",
            vec![file(
                "fresh/pricing.rs",
                "pub struct RecordedPriceList(pub Vec<Json>);\nfn items(r: &Json) -> &[Json] {\n    match r.get(\"Items\") { Some(Json::Array(v)) => v, _ => &[] }\n}",
            )],
        ),
        (
            "#4773",
            "marker above a function that holds nested readers",
            one(format!(
                "{use_line}\n// recorded-shape: the snapshot readers of the command's own file\nfn readers() {{\n    fn items(r: &Json) -> &[Json] {{\n        match r.get(\"Items\") {{ Some(Json::Array(v)) => v, _ => &[] }}\n    }}\n    fn objects(r: &Json) -> bool {{ matches!(r, Json::Object(_)) }}\n}}"
            )),
        ),
        (
            "#4773",
            "marker above a closure that holds a nested reader",
            one(format!(
                "{use_line}\nfn g(r: &Json) -> usize {{\n    // recorded-shape: a closure over the snapshot file the user supplied.\n    let size = |v: &Json| {{\n        fn inner(v: &Json) -> usize {{ match v {{ Json::Array(items) => items.len(), _ => 0 }} }}\n        inner(v)\n    }};\n    size(r)\n}}"
            )),
        ),
        (
            "#4773",
            "filler reason",
            one(format!(
                "{use_line}\n// recorded-shape: this is fine, see above\n{ITEMS_READ}"
            )),
        ),
        (
            "#4774",
            "brace form behind & in a brace-form Some",
            one(format!(
                "{use_line}\nfn items(r: &Json) -> &[Json] {{\n    match r.get(\"Items\") {{ Some {{ 0: &Json::Array {{ 0: ref v }} }} => v, _ => &[] }}\n}}"
            )),
        ),
        (
            "#4774",
            "brace form behind & in a struct-field pattern",
            one(format!(
                "{use_line}\nstruct Holder<'a> {{ body: &'a Json }}\nfn object(h: Holder) -> bool {{\n    matches!(h, Holder {{ body: &Json::Object {{ .. }} }})\n}}"
            )),
        ),
        (
            "#4775",
            "parenthesized type alias",
            one(format!(
                "{use_line}\ntype J = (Json);\nfn items(r: &J) -> &[J] {{\n    match r.get(\"Items\") {{ Some(J::Array(v)) => v, _ => &[] }}\n}}"
            )),
        ),
        (
            "#4775",
            "self rename of the JSON type in a group",
            one(format!(
                "{use_line}\nuse toolkit_core::Json::{{self as J}};\nfn items(r: &J) -> &[J] {{\n    match r.get(\"Items\") {{ Some(J::Array(v)) => v, _ => &[] }}\n}}"
            )),
        ),
    ]
}

#[test]
fn every_round_four_evasion_fails_the_whole_contract() {
    for (finding, label, files) in fourth_review_evasions() {
        let report = analyse(&files);
        assert!(
            report.scope.contains_key("fresh"),
            "{finding} {label}: {:?}",
            report.scope
        );
        assert!(
            !report.is_clean(),
            "accepted: {finding} {label}\n{report:?}"
        );
    }
}

#[test]
fn each_round_four_evasion_is_refused_for_what_it_hides() {
    // (label, reads, aliases, refused markers, unadopted)
    let expected: [(&str, &[usize], usize, usize, bool); 11] = [
        (
            "variant behind & in a struct-field pattern",
            &[4],
            0,
            0,
            false,
        ),
        ("pattern forwarded through a local macro", &[4], 0, 0, false),
        // The macro body and the pattern macro at its use.
        ("pattern built by a local macro", &[2, 4], 0, 0, false),
        (
            "replay type without Client, Step or Queue",
            &[3],
            0,
            0,
            true,
        ),
        // Neither nested function is covered by the marker above `readers`.
        (
            "marker above a function that holds nested readers",
            &[5, 7],
            0,
            0,
            false,
        ),
        (
            "marker above a closure that holds a nested reader",
            &[5],
            0,
            0,
            false,
        ),
        ("filler reason", &[4], 0, 1, false),
        (
            "brace form behind & in a brace-form Some",
            &[3],
            0,
            0,
            false,
        ),
        (
            "brace form behind & in a struct-field pattern",
            &[4],
            0,
            0,
            false,
        ),
        ("parenthesized type alias", &[4], 1, 0, false),
        ("self rename of the JSON type in a group", &[4], 1, 0, false),
    ];
    let cases = fourth_review_evasions();
    assert_eq!(cases.len(), expected.len());
    for ((_, label, files), (want, reads, aliases, markers, unadopted)) in
        cases.iter().zip(expected)
    {
        assert_eq!(*label, want);
        let report = analyse(files);
        let lines: Vec<usize> = report
            .reads
            .iter()
            .map(|read| {
                read.split(':')
                    .nth(1)
                    .expect("line")
                    .parse()
                    .expect("number")
            })
            .collect();
        assert_eq!(lines, reads, "{label}: {report:?}");
        assert_eq!(report.aliases.len(), aliases, "{label}: {report:?}");
        assert_eq!(report.markers.len(), markers, "{label}: {report:?}");
        assert_eq!(
            !report.unadopted.is_empty(),
            unadopted,
            "{label}: {report:?}"
        );
    }
}

/// An elasticache-style reader of a recorded describe file: no flag, no
/// replay type, no import, so no signal can see it (#4752).
const SILENT_READER: &str = "fn groups(describe: &Json) -> &[Json] {
    match describe.get(\"security_groups\") { Some(Json::Array(v)) => v, _ => &[] }
}";

#[test]
fn the_inventory_fails_closed_on_a_slice_it_does_not_classify() {
    let files = || {
        vec![
            file("fresh/audit.rs", SILENT_READER),
            file("plain/mod.rs", "pub fn run() {}"),
            file("lib.rs", "pub mod fresh; pub mod plain;"),
        ]
    };
    let plain = (
        "plain",
        Class::NotRecorded("the repository manifests it validates"),
    );
    // The signal scope (and the scanner of b8204e9d) never sees the reader.
    assert!(analyse(&files()).is_clean());
    // Unclassified: refused. The crate root needs no entry.
    let report = analyse_with(&files(), Some(&[plain]));
    assert_eq!(
        report.inventory,
        vec![
            "unclassified slice fresh: add it to the inventory as recorded, pending or not \
             recorded"
        ]
    );
    // Classified as recorded: scanned without any signal, and must adopt.
    let report = analyse_with(&files(), Some(&[("fresh", Class::Recorded), plain]));
    assert_eq!(report.scope["fresh"], BTreeSet::from(["inventory"]));
    assert_eq!(report.unadopted, vec!["fresh"]);
    assert_eq!(report.reads.len(), 1, "{report:?}");
    assert!(report.inventory.is_empty(), "{report:?}");
    // Pending: its findings are counted, not reported, and must match.
    let pending = |findings| {
        analyse_with(
            &files(),
            Some(&[
                (
                    "fresh",
                    Class::Pending {
                        issue: "#1",
                        findings,
                    },
                ),
                plain,
            ]),
        )
    };
    assert!(pending(1).is_clean(), "{:?}", pending(1));
    for moved in [0, 2] {
        let report = pending(moved);
        assert_eq!(report.inventory.len(), 1, "{report:?}");
        assert!(report.inventory[0].starts_with("pending slice fresh (#1) has 1 findings"));
    }
}

#[test]
fn a_not_recorded_entry_must_hold() {
    let priced = || {
        vec![file(
            "fresh/pricing.rs",
            "use toolkit_aws::recorded::Step;\npub struct RecordedPriceList;",
        )]
    };
    let report = analyse_with(
        &priced(),
        Some(&[(
            "fresh",
            Class::NotRecorded("a price table the user exports"),
        )]),
    );
    assert_eq!(report.inventory.len(), 1, "{report:?}");
    assert!(report.inventory[0].starts_with("fresh is classified not recorded but shows"));
    let quiet = || vec![file("fresh/mod.rs", "pub fn run() {}")];
    let report = analyse_with(&quiet(), Some(&[("fresh", Class::NotRecorded("todo"))]));
    assert_eq!(report.inventory.len(), 1, "{report:?}");
    assert!(report.inventory[0].contains("without a specific reason"));
    let report = analyse_with(
        &quiet(),
        Some(&[
            (
                "fresh",
                Class::NotRecorded("the manifests of the repository"),
            ),
            ("gone", Class::Recorded),
        ]),
    );
    assert_eq!(
        report.inventory,
        vec!["stale inventory entry gone: no such slice"]
    );
}

#[test]
fn macro_bodies_are_parsed_when_their_grammar_is_known_and_fail_closed_otherwise() {
    let read = |body: &str| {
        let text = format!(
            "use toolkit_aws::recorded::Step;\nfn f(r: &Json, l: Vec<Json>) {{\n    {body};\n}}"
        );
        analyse_one("fresh/m.rs", &text).reads.len()
    };
    // Constructions inside the standard expression macros are not reads.
    for body in [
        "let _ = vec![Json::Array(l.clone())]",
        "let _ = vec![Json::Object(Vec::new()); 2]",
        "assert_eq!(r, &Json::Array(l))",
        "println!(\"{:?}\", Json::Object(Vec::new()))",
        "let _ = format!(\"{x:?}\", x = Json::Array(l))",
    ] {
        assert_eq!(read(body), 0, "{body}");
    }
    // A pattern inside them is, and so is anything in an unknown macro body.
    for body in [
        "assert!(matches!(r, Json::Array(_)))",
        "debug_assert!(if let Json::Object(m) = r { m.is_empty() } else { false })",
        "custom!(Json::Array(l))",
        "custom!(r.as_array())",
        "matches!(r, Json::Object { .. } if true)",
    ] {
        assert_eq!(read(body), 1, "{body}");
    }
    // A file of a recorded slice that does not parse is never accepted.
    let report = analyse_one(
        "fresh/broken.rs",
        "use toolkit_aws::recorded::Step;\nfn f( {",
    );
    assert_eq!(report.unparsed.len(), 1, "{report:?}");
    assert!(!report.is_clean());
}

// ------------------------------------- #4861-#4864: cutting the classes

/// The fixture of #4864: a pending slice whose one finding line gains a second
/// read beside the first.
const SAME_LINE_READS: &str = "fn items(r: &Json) -> usize {
    match r.get(\"A\") { Some(Json::Object(p)) => { let _ = match r.get(\"Injected\") { Some(Json::Array(v)) => &v[..], _ => &[] }; p.len() } _ => 0 }
}";

/// Every reviewer reproduction of #4852 against 2d3904a1: each one passed
/// that scanner's whole contract (scoped by `[("fresh", Class::Recorded)]`)
/// and fails this one. `(finding, label, files)`.
fn fifth_review_evasions() -> Vec<(&'static str, &'static str, Vec<SourceFile>)> {
    let one = |text: &str| vec![file("fresh/mod.rs", text)];
    vec![
        (
            "#4861",
            "raw identifier variant, tuple form",
            one(r#"use toolkit_aws::recorded::Step;
fn response_items(r: &Json) -> &[Json] {
    match r.get("Items") { Some(Json::r#Array(v)) => v, _ => &[] }
}"#),
        ),
        (
            "#4861",
            "raw identifier variant, brace form",
            one(r#"use toolkit_aws::recorded::Step;
fn response_items(r: &Json) -> &[Json] {
    match r.get("Items") { Some(Json::r#Array { 0: v }) => v, _ => &[] }
}"#),
        ),
        (
            "#4861",
            "raw identifier shape method",
            one(r#"use toolkit_aws::recorded::Step;
use serde_json::Value;
fn response_items(r: &Value) -> &[Value] {
    r.get("Items").and_then(|v| v.r#as_array()).map(Vec::as_slice).unwrap_or(&[])
}"#),
        ),
        (
            "#4862",
            "local macro taking the variant as an argument",
            one(r#"use toolkit_aws::recorded::Step;
macro_rules! extract {
    ($variant:ident, $value:expr) => {
        match $value {
            Some(Json::$variant(items)) => items,
            _ => &[],
        }
    };
}
fn response_items(r: &Json) -> &[Json] {
    extract!(Array, r.get("Items"))
}"#),
        ),
        (
            "#4862",
            "local macro shadowing vec",
            one(r#"use toolkit_aws::recorded::Step;
macro_rules! vec {
    ($variant:ident, $value:expr) => {
        match $value {
            Some(Json::$variant(items)) => items,
            _ => &[],
        }
    };
}
fn response_items(r: &Json) -> &[Json] {
    vec!(Array, r.get("Items"))
}"#),
        ),
        (
            "#4863",
            "qualified path to a marked variant re-export",
            one(r#"use toolkit_aws::recorded::Step;
mod variants {
    // recorded-shape: exported variant used for construction
    pub use toolkit_core::Json::Array as List;
}
fn response_items(r: &Json) -> &[Json] {
    match r.get("Items") { Some(variants::List(v)) => v, _ => &[] }
}"#),
        ),
        (
            "#4863",
            "variant renamed in another file",
            vec![
                file(
                    "fresh/mod.rs",
                    r#"use toolkit_aws::recorded::Step;
use crate::fresh::variants::List;
fn response_items(r: &Json) -> &[Json] {
    match r.get("Items") { Some(List(v)) => v, _ => &[] }
}"#,
                ),
                file(
                    "fresh/variants.rs",
                    "// recorded-shape: exported variant used for construction\npub use toolkit_core::Json::Array as List;",
                ),
            ],
        ),
    ]
}

fn fresh_recorded(files: &[SourceFile]) -> Report {
    analyse_with(files, Some(&[("fresh", Class::Recorded)]))
}

#[test]
fn every_round_five_evasion_fails_the_whole_contract() {
    for (finding, label, files) in fifth_review_evasions() {
        let report = fresh_recorded(&files);
        assert!(
            report.unadopted.is_empty() && report.inventory.is_empty(),
            "{finding} {label}: {report:?}"
        );
        assert!(
            !report.is_clean(),
            "accepted: {finding} {label}\n{report:?}"
        );
    }
}

#[test]
fn each_round_five_evasion_is_refused_for_what_it_hides() {
    let at = |entries: &[String]| -> Vec<String> {
        entries
            .iter()
            .map(|entry| entry.splitn(3, ':').take(2).collect::<Vec<_>>().join(":"))
            .collect()
    };
    // (label, reads, macros) as `file:line`.
    let expected: [(&str, &[&str], &[&str]); 7] = [
        (
            "raw identifier variant, tuple form",
            &["fresh/mod.rs:3"],
            &[],
        ),
        (
            "raw identifier variant, brace form",
            &["fresh/mod.rs:3"],
            &[],
        ),
        ("raw identifier shape method", &["fresh/mod.rs:4"], &[]),
        // The definition and the invocation; the scanner never expands it.
        (
            "local macro taking the variant as an argument",
            &[],
            &["fresh/mod.rs:2", "fresh/mod.rs:11"],
        ),
        (
            "local macro shadowing vec",
            &[],
            &["fresh/mod.rs:2", "fresh/mod.rs:11"],
        ),
        // The marked re-export allows its own line, never the reader.
        (
            "qualified path to a marked variant re-export",
            &["fresh/mod.rs:7"],
            &[],
        ),
        ("variant renamed in another file", &["fresh/mod.rs:4"], &[]),
    ];
    let cases = fifth_review_evasions();
    assert_eq!(cases.len(), expected.len());
    for ((_, label, files), (want, reads, macros)) in cases.iter().zip(expected) {
        assert_eq!(*label, want);
        let report = fresh_recorded(files);
        assert_eq!(at(&report.reads), reads, "{label}: {report:?}");
        assert_eq!(at(&report.macros), macros, "{label}: {report:?}");
        assert!(
            report.aliases.is_empty() && report.markers.is_empty() && report.helpers.is_empty(),
            "{label}: {report:?}"
        );
    }
}

#[test]
fn raw_identifiers_are_the_names_they_spell() {
    let report = fresh_recorded(&[file(
        "fresh/mod.rs",
        r"use toolkit_aws::r#recorded::Step;
use toolkit_core::r#Json as r#J;
fn r#optional_list() {}
fn f(r: &J) -> bool { matches!(r, J::r#Object(_)) || r.r#as_object().is_some() }
fn r#match(r#fn: &Json) -> usize { r#fn.len() }",
    )]);
    assert!(report.scope["fresh"].contains("import"), "{report:?}");
    assert_eq!(report.helpers, vec!["fresh/mod.rs: fn optional_list"]);
    assert_eq!(report.aliases.len(), 1, "{report:?}");
    assert_eq!(
        report.reads,
        vec![
            "fresh/mod.rs:4: fn f(r: &J) -> bool { matches!(r, J::r#Object(_)) || \
              r.r#as_object().is_some() } [2 on this line]"
        ]
    );
}

#[test]
fn a_local_macro_needs_a_marked_definition_wherever_it_is_defined() {
    let reader = "use toolkit_aws::recorded::Step;\nfn f(r: &Json) -> usize {\n    count!(r)\n}";
    let defined = |marker: &str| {
        file(
            "other/mod.rs",
            &format!("{marker}\nmacro_rules! count {{ ($v:expr) => {{ $v.len() }}; }}"),
        )
    };
    let inventory = [
        ("fresh", Class::Recorded),
        (
            "other",
            Class::NotRecorded("the manifests of the repository"),
        ),
    ];
    // Defined in a slice the contract does not scan: its use still counts.
    let report = analyse_with(
        &[file("fresh/mod.rs", reader), defined("")],
        Some(&inventory),
    );
    assert_eq!(
        report.macros,
        vec!["fresh/mod.rs:3: count!(r)"],
        "{report:?}"
    );
    // A marked definition allows every invocation.
    let marked = defined("// recorded-shape: counts the members of a list the command built");
    let report = analyse_with(&[file("fresh/mod.rs", reader), marked], Some(&inventory));
    assert!(report.is_clean(), "{report:?}");
    // A refused marker allows nothing.
    let report = analyse_with(
        &[
            file("fresh/mod.rs", reader),
            defined("// recorded-shape: todo"),
        ],
        Some(&inventory),
    );
    assert_eq!(report.macros.len(), 1, "{report:?}");
    // A definition in a recorded slice is itself a finding, also inside a
    // function, and a marker on it allows both.
    let local = "use toolkit_aws::recorded::Step;\nfn f(r: &Json) -> usize {\n    macro_rules! count { ($v:expr) => { $v.len() }; }\n    count!(r)\n}";
    let report = fresh_recorded(&[file("fresh/mod.rs", local)]);
    assert_eq!(
        report.macros,
        vec![
            "fresh/mod.rs:3: macro_rules! count { ($v:expr) => { $v.len() }; }",
            "fresh/mod.rs:4: count!(r)"
        ],
        "{report:?}"
    );
    let marked = local.replace(
        "    macro_rules!",
        "    // recorded-shape: counts the members of a list the command built\n    macro_rules!",
    );
    assert!(fresh_recorded(&[file("fresh/mod.rs", &marked)]).is_clean());
    // The standard macros keep their grammar by their plain or `std::` path;
    // any other path is opaque.
    let body = |call: &str| {
        let text = format!(
            "use toolkit_aws::recorded::Step;\nfn f(l: Vec<Json>) {{\n    let _ = {call};\n}}"
        );
        fresh_recorded(&[file("fresh/mod.rs", &text)]).reads.len()
    };
    assert_eq!(body("vec![Json::Array(l)]"), 0);
    assert_eq!(body("std::vec![Json::Array(l)]"), 0);
    assert_eq!(body("other::vec![Json::Array(l)]"), 1);
}

#[test]
fn a_renamed_variant_is_followed_across_files_and_renames() {
    let files = [
        file(
            "fresh/mod.rs",
            "use toolkit_aws::recorded::Step;\nfn f(r: &Json) -> bool {\n    matches!(r, crate::other::Seq(_)) || matches!(r, Map { .. })\n}",
        ),
        file(
            "other/mod.rs",
            "pub use toolkit_core::Json::Array as List;\npub use self::List as Seq;\npub use serde_json::Value::r#Object as Map;",
        ),
    ];
    let report = analyse_with(
        &files,
        Some(&[
            ("fresh", Class::Recorded),
            (
                "other",
                Class::NotRecorded("the manifests of the repository"),
            ),
        ]),
    );
    assert_eq!(
        report.reads,
        vec![
            "fresh/mod.rs:3: matches!(r, crate::other::Seq(_)) || matches!(r, Map { .. }) \
             [2 on this line]"
        ],
        "{report:?}"
    );
}

#[test]
fn the_pending_ratchet_counts_each_read_not_each_line() {
    let pending = |findings| {
        analyse_with(
            &[file("fresh/mod.rs", SAME_LINE_READS)],
            Some(&[(
                "fresh",
                Class::Pending {
                    issue: "#1",
                    findings,
                },
            )]),
        )
    };
    // 2d3904a1 counted the line once and accepted 1.
    let report = pending(1);
    assert_eq!(report.inventory.len(), 1, "{report:?}");
    assert!(report.inventory[0].starts_with("pending slice fresh (#1) has 2 findings"));
    assert!(pending(2).is_clean(), "{:?}", pending(2));
    // Recorded, the two reads print as one line that says how many it holds.
    let report = fresh_recorded(&[file("fresh/mod.rs", SAME_LINE_READS)]);
    assert_eq!(report.reads.len(), 1, "{report:?}");
    assert!(report.reads[0].ends_with("[2 on this line]"), "{report:?}");
    // Two aliases on one line are two findings as well.
    let aliases = "type A = Json; type B = Json;";
    let report = analyse_with(
        &[file("fresh/mod.rs", aliases)],
        Some(&[(
            "fresh",
            Class::Pending {
                issue: "#1",
                findings: 2,
            },
        )]),
    );
    assert!(report.is_clean(), "{report:?}");
}

#[test]
fn a_read_added_beside_an_accessor_read_fails_the_real_contract() {
    // The reviewer mutation of #4864 over the real sources: an unmarked
    // member read on the line of an existing `bedrock_resolve` read. #4839
    // moved that slice from `Pending` to `Recorded`, so the added read is
    // now reported by itself instead of raising the pending count.
    let mut files = sources();
    let pricing = files
        .iter_mut()
        .find(|file| file.relative == "bedrock_resolve/pricing.rs")
        .expect("bedrock_resolve/pricing.rs");
    let before = pricing.text.clone();
    pricing.text = before.replacen(
        "if object_members(item).is_none() {",
        "if object_members(item).is_none() { let _ = match item.get(\"Injected\") { Some(Json::Array(v)) => &v[..], _ => &[] };",
        1,
    );
    assert_ne!(pricing.text, before, "the mutation site moved");
    let report = analyse_with(&files, Some(INVENTORY));
    assert!(report.inventory.is_empty(), "{report:?}");
    assert_eq!(report.reads.len(), 1, "{report:?}");
    assert!(
        report.reads[0].starts_with("bedrock_resolve/pricing.rs:")
            && report.reads[0].contains("Injected"),
        "{report:?}"
    );
}

#[test]
fn an_out_of_line_cfg_test_module_is_test_code_whatever_its_name() {
    // #4869: `#[cfg(test)] mod tests_core;` brings in `tests_core.rs` (or
    // `tests_core/mod.rs`) and its own submodules; none is production code.
    let files = vec![
        file(
            "fresh/mod.rs",
            "use toolkit_aws::recorded::Step;\n#[cfg(test)]\nmod tests_core;\n#[cfg(test)]\nmod fixtures;\nmod inner {\n    #[cfg(test)]\n    mod deep;\n}\n#[cfg(test)]\nmod checks {\n    mod helper;\n}\n#[cfg(test)]\n#[path = \"mocks/fake.rs\"]\nmod fake;\nmod live;\nmod sub;",
        ),
        file("fresh/tests_core.rs", TEST_MODULE_READ),
        file(
            "fresh/fixtures/mod.rs",
            &format!("mod more;\n{TEST_MODULE_READ}"),
        ),
        file("fresh/fixtures/more.rs", TEST_MODULE_READ),
        file("fresh/inner/deep.rs", TEST_MODULE_READ),
        // The out-of-line child of an inline test module.
        file("fresh/checks/helper.rs", TEST_MODULE_READ),
        // A `#[path]` test module, and its child beside it (a mod-rs file).
        file(
            "fresh/mocks/fake.rs",
            &format!("mod twin;\n{TEST_MODULE_READ}"),
        ),
        file("fresh/mocks/twin.rs", TEST_MODULE_READ),
        file("fresh/sub.rs", "#[cfg(test)]\nmod check;\npub fn run() {}"),
        file("fresh/sub/check.rs", TEST_MODULE_READ),
        // Production modules beside them still count, and so does a file no
        // declaration in sight loads.
        file("fresh/live.rs", TEST_MODULE_READ),
        file("fresh/tests_core_live.rs", TEST_MODULE_READ),
        // A top-level test module of the crate root.
        file("lib.rs", "pub mod fresh;\n#[cfg(test)]\nmod crate_checks;"),
        file("crate_checks.rs", TEST_MODULE_READ),
    ];
    let report = analyse_with(&files, Some(&[("fresh", Class::Recorded)]));
    assert_eq!(
        read_files(&report),
        vec!["fresh/live.rs", "fresh/tests_core_live.rs"],
        "{report:?}"
    );
    // `crate_checks` is test code, so it needs no inventory entry.
    assert!(report.inventory.is_empty(), "{report:?}");
}

/// A production-shaped raw read for the test-module fixtures.
const TEST_MODULE_READ: &str = "fn response_items(r: &Json) -> &[Json] {\n    match r.get(\"Items\") { Some(Json::Array(v)) => v, _ => &[] }\n}";

/// The files of `report`'s reads, sorted.
fn read_files(report: &Report) -> Vec<&str> {
    let mut at: Vec<&str> = report
        .reads
        .iter()
        .map(|read| read.split(':').next().expect("file"))
        .collect();
    at.sort_unstable();
    at
}

#[test]
fn an_inline_test_mock_does_not_exclude_the_production_module() {
    // #4872, the reviewer's reproduction: production loads `transport.rs`,
    // test builds swap in an inline mock of the same name. The mock claims no
    // file, so the production read is still refused.
    let files = vec![
        file(
            "fresh/mod.rs",
            "use toolkit_aws::recorded::Step;\n#[cfg(not(test))]\nmod transport;\n#[cfg(test)]\nmod transport { pub fn mock() {} }",
        ),
        file("fresh/transport.rs", TEST_MODULE_READ),
    ];
    let report = analyse_with(&files, Some(&[("fresh", Class::Recorded)]));
    assert!(!report.is_clean(), "{report:?}");
    assert_eq!(
        read_files(&report),
        vec!["fresh/transport.rs"],
        "{report:?}"
    );
}

#[test]
fn a_path_selected_test_mock_does_not_exclude_the_production_module() {
    // #4872: the test build loads `mock_transport.rs` through `#[path]`, so
    // the default `transport.rs` stays production code.
    let files = vec![
        file(
            "fresh/mod.rs",
            "use toolkit_aws::recorded::Step;\n#[cfg(not(test))]\nmod transport;\n#[cfg(test)]\n#[path = \"mock_transport.rs\"]\nmod transport;",
        ),
        file("fresh/transport.rs", TEST_MODULE_READ),
        file("fresh/mock_transport.rs", "pub fn mock() {}"),
    ];
    let report = analyse_with(&files, Some(&[("fresh", Class::Recorded)]));
    assert!(!report.is_clean(), "{report:?}");
    assert_eq!(
        read_files(&report),
        vec!["fresh/transport.rs"],
        "{report:?}"
    );
}

#[test]
fn a_file_any_non_test_declaration_loads_stays_scanned() {
    // #4872: a file is test code only when every declaration that loads it
    // is test-only; `cfg(not(test))`, no `cfg` or another `cfg` keeps it.
    for production in [
        "#[cfg(not(test))]\n#[path = \"shared.rs\"]\nmod shared_live;",
        "#[path = \"shared.rs\"]\nmod shared_live;",
        "#[cfg(feature = \"live\")]\n#[path = \"shared.rs\"]\nmod shared_live;",
        "#[cfg(all(test, unix))]\n#[path = \"shared.rs\"]\nmod shared_live;",
    ] {
        let files = vec![
            file(
                "fresh/mod.rs",
                &format!(
                    "use toolkit_aws::recorded::Step;\n#[cfg(test)]\nmod shared;\n{production}"
                ),
            ),
            file("fresh/shared.rs", TEST_MODULE_READ),
        ];
        let report = analyse_with(&files, Some(&[("fresh", Class::Recorded)]));
        assert_eq!(
            read_files(&report),
            vec!["fresh/shared.rs"],
            "{production}: {report:?}"
        );
    }
    // A production file under a test-only parent is test code; one a
    // production parent also loads is not.
    let files = vec![
        file(
            "fresh/mod.rs",
            "use toolkit_aws::recorded::Step;\n#[cfg(test)]\nmod checks;\n#[path = \"checks/kept.rs\"]\nmod kept;",
        ),
        file("fresh/checks.rs", "mod dropped;\nmod kept;"),
        file("fresh/checks/dropped.rs", TEST_MODULE_READ),
        file("fresh/checks/kept.rs", TEST_MODULE_READ),
    ];
    let report = analyse_with(&files, Some(&[("fresh", Class::Recorded)]));
    assert_eq!(
        read_files(&report),
        vec!["fresh/checks/kept.rs"],
        "{report:?}"
    );
}

#[test]
fn a_function_local_production_module_keeps_its_file_scanned() {
    // #4880, the reviewer's reproduction: a production `#[path]` module in a
    // function body loads the file a top-level `#[cfg(test)]` module loads.
    let reproduction = "use toolkit_aws::recorded::Step;\n#[cfg(test)]\nmod shared;\npub fn run() {\n    #[path = \"shared.rs\"]\n    mod live;\n    live::run();\n}";
    let report = analyse_with(
        &[
            file("fresh/mod.rs", reproduction),
            file("fresh/shared.rs", TEST_MODULE_READ),
        ],
        Some(&[("fresh", Class::Recorded)]),
    );
    assert!(!report.is_clean(), "{report:?}");
    assert_eq!(read_files(&report), vec!["fresh/shared.rs"], "{report:?}");
    // The same miss in a constant block, a static, a method, a nested block,
    // under `cfg(feature)` or `cfg(all(test, ..))`, in an inline module's
    // function, or in a default-path declaration rustc refuses in a block
    // (fail safe), or in a macro's tokens.
    for production in [
        "const _: () = { #[path = \"shared.rs\"] mod live; };",
        "pub fn run() { let _ = const { #[path = \"shared.rs\"] mod live; 0 }; }",
        "static LIVE: u8 = { #[path = \"shared.rs\"] mod live; 0 };",
        "pub struct S;\nimpl S {\n    pub fn run(&self) { #[path = \"shared.rs\"] mod live; }\n}",
        "pub fn run() { if true { loop { #[path = \"shared.rs\"] mod live; break; } } }",
        "pub fn run() {\n    #[cfg(feature = \"live\")]\n    #[path = \"shared.rs\"]\n    mod live;\n}",
        "pub fn run() {\n    #[cfg(all(test, unix))]\n    #[path = \"shared.rs\"]\n    mod live;\n}",
        "mod outer {\n    pub fn run() { #[path = \"../shared.rs\"] mod live; }\n}",
        "pub fn run() { mod shared; }",
        "pub fn run() { wrap! { #[path = \"shared.rs\"] mod live; } }",
    ] {
        let files = vec![
            file(
                "fresh/mod.rs",
                &format!("use toolkit_aws::recorded::Step;\n#[cfg(test)]\nmod shared;\n{production}"),
            ),
            file("fresh/shared.rs", TEST_MODULE_READ),
        ];
        let report = analyse_with(&files, Some(&[("fresh", Class::Recorded)]));
        assert_eq!(
            read_files(&report),
            vec!["fresh/shared.rs"],
            "{production}: {report:?}"
        );
    }
    // In a block of a non-mod-rs `fresh/sub.rs`, `#[path]` resolves against
    // `fresh`, as rustc does.
    let files = vec![
        file(
            "fresh/mod.rs",
            "use toolkit_aws::recorded::Step;\nmod sub;\n#[cfg(test)]\nmod shared;",
        ),
        file(
            "fresh/sub.rs",
            "pub fn run() { #[path = \"shared.rs\"] mod live; }",
        ),
        file("fresh/shared.rs", TEST_MODULE_READ),
    ];
    let report = analyse_with(&files, Some(&[("fresh", Class::Recorded)]));
    assert_eq!(read_files(&report), vec!["fresh/shared.rs"], "{report:?}");
}

#[test]
fn a_function_local_test_module_is_test_code() {
    // #4880: a nested declaration is test-only under its own `#[cfg(test)]`
    // or an enclosing `#[cfg(test)]` item; a lone test loader drops the file.
    for test in [
        "pub fn run() {\n    #[cfg(test)]\n    #[path = \"mocks/fake.rs\"]\n    mod fake;\n}",
        "#[cfg(test)]\nfn helper() { #[path = \"mocks/fake.rs\"] mod fake; }",
        "#[cfg(test)]\nconst _: () = { #[path = \"mocks/fake.rs\"] mod fake; };",
        "pub struct S;\nimpl S {\n    #[cfg(test)]\n    fn helper(&self) { #[path = \"mocks/fake.rs\"] mod fake; }\n}",
        "#[cfg(test)]\nmod checks {\n    fn helper() { #[path = \"../mocks/fake.rs\"] mod fake; }\n}",
    ] {
        let files = vec![
            file(
                "fresh/mod.rs",
                &format!("use toolkit_aws::recorded::Step;\nmod live;\n{test}"),
            ),
            file("fresh/live.rs", TEST_MODULE_READ),
            file("fresh/mocks/fake.rs", TEST_MODULE_READ),
        ];
        let report = analyse_with(&files, Some(&[("fresh", Class::Recorded)]));
        assert_eq!(read_files(&report), vec!["fresh/live.rs"], "{test}: {report:?}");
    }
}

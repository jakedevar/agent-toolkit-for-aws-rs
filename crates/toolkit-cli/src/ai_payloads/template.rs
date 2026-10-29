//! Template grammar, placeholder lexer and classification (#4386, slice S1).
//!
//! One template is a `code_templates/*.py` file of the `aws-ai-ml` skill. This
//! module reads its text and reports, without writing or executing anything:
//!
//! - the **cells** (`# Cell N[ [markdown]]: Label` markers, section 4.3 rules
//!   1 to 3 of `docs/migration/ai-payloads-design.md`);
//! - every **placeholder occurrence** (`[NAME]`), classified by a small Python
//!   lexer as a quoted slot, a bare slot, a comment or a sentinel (rule 4),
//!   with the secret (rule 5) and typed-bare-slot (rule 6) checks;
//! - the **source-text policy** (rule 7);
//! - the literal assignments `assign` may later set (section 4.4.3, listed as
//!   *targets*) and the comments that ask the agent for a manual edit.
//!
//! The lexer tracks strings (single, triple quoted, prefixed), comments and
//! bracket depth, nothing more. It never evaluates the template. Everything it
//! reports is a position (line and 1-based character column) or a name; no
//! diagnostic quotes a line of the template (section 4.2: diagnostics name
//! placeholders, cells and positions, never values).

use std::collections::{BTreeMap, BTreeSet};

use toolkit_core::is_unsafe_char;

/// The bare-slot names that exist in the 23 real templates (section 1.4). A
/// bare slot is a typed expression, so a name without a row in the type table
/// of section 4.4.1 is a lint error (rule 6). The table itself (`types.rs`) is
/// slice S2b; until then this closed list is the type table's name column.
pub(crate) const TYPED_BARE_SLOTS: [&str; 9] = [
    "ACCEPT_EULA",
    "CONCURRENCY",
    "INPUT_TOKENS_MEAN",
    "INPUT_TOKENS_STDDEV",
    "METRICS_LIST",
    "OUTPUT_TOKENS_MEAN",
    "OUTPUT_TOKENS_STDDEV",
    "REQUEST_COUNT",
    "TRUE_OR_FALSE",
];

/// Characters of a comment kept for a manual edit point.
const MANUAL_TEXT_CHARS: usize = 60;

/// The words that mark a comment as a manual edit point (section 1.4).
const MANUAL_MARKERS: [&str; 2] = ["NOTE FOR AGENT", "Uncomment"];

/// How a placeholder occurrence is used in the template text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Context {
    /// A string literal whose whole body is the placeholder.
    Quoted,
    /// The placeholder as an expression token in code.
    Bare,
    /// Inside a `#` comment.
    Comment,
    /// A quoted occurrence that is an operand of `==` or `!=`, or an element of
    /// the bracketed right operand of `in` (T17:39). Never substituted.
    Sentinel,
}

impl Context {
    /// The name used in the catalog.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Quoted => "quoted",
            Self::Bare => "bare",
            Self::Comment => "comment",
            Self::Sentinel => "sentinel",
        }
    }
}

/// One occurrence of `[NAME]`.
#[derive(Debug, Clone)]
pub(crate) struct Occurrence {
    /// The placeholder name, without the brackets.
    pub name: String,
    pub context: Context,
    /// The quote character of a quoted or sentinel occurrence.
    pub quote: Option<char>,
    /// 1-based line.
    pub line: usize,
    /// 1-based character column: the opening quote of a quoted or sentinel
    /// occurrence, the `[` of any other.
    pub column: usize,
}

/// The type of value a placeholder takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SlotKind {
    /// A quoted slot: a string value. Also the kind of a name that only occurs
    /// in comments.
    String,
    /// A bare slot: a typed expression (section 4.4.1).
    Value,
    /// A secret slot (rule 5).
    Secret,
}

impl SlotKind {
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::String => "string",
            Self::Value => "value",
            Self::Secret => "secret",
        }
    }
}

/// The occurrences of one placeholder name, summarized.
#[derive(Debug, Clone)]
pub(crate) struct Slot {
    pub name: String,
    pub kind: SlotKind,
    /// Quoted and bare occurrences: the places a value is written.
    pub code: usize,
    pub sentinel: usize,
    pub comment: usize,
}

impl Slot {
    /// A placeholder that only occurs in comments (no value is ever written).
    pub(crate) const fn comment_only(&self) -> bool {
        self.code == 0 && self.sentinel == 0 && self.comment > 0
    }
}

/// One `# Cell N: Label` section.
#[derive(Debug, Clone)]
pub(crate) struct Cell {
    pub number: usize,
    pub label: String,
    pub markdown: bool,
    /// The label carried the `# NOTEBOOK_ONLY_SECTION` suffix.
    pub notebook_only_section: bool,
    /// 1-based line of the marker.
    pub line: usize,
    /// Body lines after trimming leading and trailing blank lines.
    pub lines: usize,
    /// Body lines ending in `# NOTEBOOK_ONLY`.
    pub notebook_only_lines: usize,
    /// The content of a markdown cell: its comment lines without `# `.
    pub text: Option<String>,
}

/// What kind of literal a target holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LiteralKind {
    String,
    Int,
    Float,
    Bool,
}

impl LiteralKind {
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::String => "string",
            Self::Int => "int",
            Self::Float => "float",
            Self::Bool => "bool",
        }
    }
}

/// A literal configuration field `assign` may set (section 4.4.3).
#[derive(Debug, Clone)]
pub(crate) struct Target {
    pub name: String,
    pub cell: usize,
    /// 1-based line of the `NAME =` statement.
    pub line: usize,
    pub kind: LiteralKind,
    /// The literal as written.
    pub literal: String,
}

/// A comment that asks the agent for a manual edit.
#[derive(Debug, Clone)]
pub(crate) struct ManualEdit {
    pub cell: Option<usize>,
    pub line: usize,
    pub text: String,
}

/// A grammar violation (`templates --check`).
#[derive(Debug, Clone)]
pub(crate) struct Violation {
    pub code: &'static str,
    /// 1-based line, or 0 when the violation concerns the whole file.
    pub line: usize,
    /// 1-based character column, or 0 when not applicable.
    pub column: usize,
    pub message: String,
}

/// Everything read from one template.
#[derive(Debug, Clone, Default)]
pub(crate) struct Template {
    pub title: String,
    pub cells: Vec<Cell>,
    pub slots: Vec<Slot>,
    pub occurrences: Vec<Occurrence>,
    pub targets: Vec<Target>,
    pub manual_edits: Vec<ManualEdit>,
    pub violations: Vec<Violation>,
}

/// Whether `name` is a secret slot: a `_`-delimited segment `TOKEN`, `SECRET`,
/// `PASSWORD`, `CREDENTIAL` or `CREDENTIALS` (rule 5).
pub(crate) fn is_secret_name(name: &str) -> bool {
    name.split('_').any(|segment| {
        matches!(
            segment,
            "TOKEN" | "SECRET" | "PASSWORD" | "CREDENTIAL" | "CREDENTIALS"
        )
    })
}

/// Reads one template. Never fails: every problem is a [`Violation`].
pub(crate) fn parse(text: &str) -> Template {
    let mut template = Template::default();
    let (text, has_bom) = match text.strip_prefix('\u{feff}') {
        Some(rest) => (rest, true),
        None => (text, false),
    };
    if has_bom {
        template
            .violations
            .push(source_char_violation(1, 1, '\u{feff}'));
    }
    let normalized = text.replace("\r\n", "\n");
    let mut lines: Vec<&str> = normalized.split('\n').collect();
    if normalized.is_empty() || normalized.ends_with('\n') {
        lines.pop();
    }
    check_source_text(&lines, &mut template.violations);

    let markers = read_markers(&lines, &mut template.violations);
    template.cells = build_cells(&lines, &markers);
    template.title = template
        .cells
        .first()
        .filter(|cell| cell.number == 0)
        .map(|cell| cell.label.clone())
        .unwrap_or_default();
    if markers.is_empty() {
        template.violations.push(Violation {
            code: "no_cells",
            line: 0,
            column: 0,
            message: "the template has no `# Cell N: Label` marker".to_string(),
        });
    }

    let lexed = lex(&normalized);
    template.violations.extend(lexed.violations);
    let tokens = lexed.tokens;
    let mut occurrences = lexed.occurrences;
    occurrences.extend(string_occurrences(&tokens));
    occurrences.sort_by_key(|occurrence| (occurrence.line, occurrence.column));

    template.slots = summarize(&occurrences);
    template.violations.extend(slot_violations(&occurrences));
    template.targets = find_targets(&tokens, &template.cells);
    template.manual_edits = lexed
        .comments
        .iter()
        .filter(|comment| {
            MANUAL_MARKERS
                .iter()
                .any(|marker| comment.text.contains(marker))
        })
        .map(|comment| manual_edit(comment, &template.cells))
        .collect();
    template.occurrences = occurrences;
    template
        .violations
        .sort_by_key(|violation| (violation.line, violation.column));
    template
}

fn source_char_violation(line: usize, column: usize, character: char) -> Violation {
    Violation {
        code: "source_char",
        line,
        column,
        message: format!(
            "U+{:04X} is not allowed in template source (rule 7: only tab and line ends)",
            u32::from(character)
        ),
    }
}

/// Rule 7: LF or CR LF line ends and tab; no other control, bidi or format
/// character, a lone CR included.
fn check_source_text(lines: &[&str], violations: &mut Vec<Violation>) {
    for (index, line) in lines.iter().enumerate() {
        for (column, character) in line.chars().enumerate() {
            if character != '\t' && is_unsafe_char(character) {
                violations.push(source_char_violation(index + 1, column + 1, character));
            }
        }
    }
}

// ------------------------------------------------------------------- cells

struct Marker {
    /// 0-based index of the marker line.
    index: usize,
    number: usize,
    markdown: bool,
    label: String,
    notebook_only_section: bool,
}

/// Reads a `# Cell N[ [markdown]]: Label` marker. A line is a marker when it
/// starts with `# Cell `, decimal digits, an optional ` [markdown]` and a
/// colon; whether the label is well formed is reported by the caller.
fn parse_marker(index: usize, line: &str) -> Option<(Marker, bool)> {
    let rest = line.strip_prefix("# Cell ")?;
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    let number = rest[..digits].parse().unwrap_or(usize::MAX);
    let mut after = &rest[digits..];
    let markdown = match after.strip_prefix(" [markdown]") {
        Some(remainder) => {
            after = remainder;
            true
        }
        None => false,
    };
    let after = after.strip_prefix(':')?;
    let (spaced, body) = match after.strip_prefix(' ') {
        Some(body) => (true, body),
        None => (false, after),
    };
    let (label, notebook_only_section) = split_section_suffix(body);
    let well_formed = spaced && !label.is_empty();
    Some((
        Marker {
            index,
            number,
            markdown,
            label,
            notebook_only_section,
        },
        well_formed,
    ))
}

/// Splits a trailing `# NOTEBOOK_ONLY_SECTION` (any spacing around the hash)
/// off a label. A hash inside the label (`C# basics`) stays.
fn split_section_suffix(body: &str) -> (String, bool) {
    let trimmed = body.trim_end();
    if let Some(head) = trimmed.strip_suffix("NOTEBOOK_ONLY_SECTION") {
        if let Some(head) = head.trim_end().strip_suffix('#') {
            if head.is_empty() || head.ends_with(char::is_whitespace) {
                return (head.trim().to_string(), true);
            }
        }
    }
    (trimmed.trim().to_string(), false)
}

fn read_markers(lines: &[&str], violations: &mut Vec<Violation>) -> Vec<Marker> {
    let mut markers: Vec<Marker> = Vec::new();
    let mut expected = 0usize;
    for (index, line) in lines.iter().enumerate() {
        let Some((marker, well_formed)) = parse_marker(index, line) else {
            continue;
        };
        let at = index + 1;
        let violation = |code: &'static str, message: String| Violation {
            code,
            line: at,
            column: 0,
            message,
        };
        if !well_formed {
            violations.push(violation(
                "cell_label_empty",
                format!("Cell {} needs `: ` and a non-empty label", marker.number),
            ));
        }
        if markers.is_empty() {
            if marker.number != 0 {
                violations.push(violation(
                    "cell_zero_missing",
                    format!(
                        "the first marker is Cell {}; Cell 0 [markdown] must come first",
                        marker.number
                    ),
                ));
            } else if !marker.markdown {
                violations.push(violation(
                    "cell_zero_not_markdown",
                    "Cell 0 must be `# Cell 0 [markdown]: Title`".to_string(),
                ));
            }
        } else {
            if marker.number < expected {
                violations.push(violation(
                    "cell_duplicate",
                    format!(
                        "Cell {} repeats a number; Cell {expected} was expected",
                        marker.number
                    ),
                ));
            } else if marker.number > expected {
                violations.push(violation(
                    "cell_gap",
                    format!(
                        "Cell {} follows Cell {}; markers number consecutively",
                        marker.number,
                        expected - 1
                    ),
                ));
            }
            if marker.markdown {
                violations.push(violation(
                    "cell_markdown_late",
                    format!("Cell {} is `[markdown]`; only Cell 0 may be", marker.number),
                ));
            }
        }
        expected = marker.number.saturating_add(1);
        markers.push(marker);
    }
    markers
}

fn build_cells(lines: &[&str], markers: &[Marker]) -> Vec<Cell> {
    let mut cells = Vec::with_capacity(markers.len());
    for (position, marker) in markers.iter().enumerate() {
        let end = markers
            .get(position + 1)
            .map_or(lines.len(), |next| next.index);
        let section = &lines[marker.index + 1..end];
        let notebook_only_lines = section
            .iter()
            .filter(|line| line.trim_end().ends_with("# NOTEBOOK_ONLY"))
            .count();
        let first = section.iter().position(|line| !line.trim().is_empty());
        let last = section.iter().rposition(|line| !line.trim().is_empty());
        let body: &[&str] = match (first, last) {
            (Some(first), Some(last)) => &section[first..=last],
            _ => &[],
        };
        let text = marker.markdown.then(|| {
            body.iter()
                .map(|line| {
                    let line: &str = line;
                    line.strip_prefix("# ")
                        .or_else(|| (line == "#").then_some(""))
                        .unwrap_or(line)
                })
                .collect::<Vec<_>>()
                .join("\n")
        });
        cells.push(Cell {
            number: marker.number,
            label: marker.label.clone(),
            markdown: marker.markdown,
            notebook_only_section: marker.notebook_only_section,
            line: marker.index + 1,
            lines: body.len(),
            notebook_only_lines,
            text,
        });
    }
    cells
}

/// The cell holding `line`: the last marker at or before it.
fn cell_at(cells: &[Cell], line: usize) -> Option<&Cell> {
    let after = cells.partition_point(|cell| cell.line <= line);
    after.checked_sub(1).map(|index| &cells[index])
}

// ------------------------------------------------------------------- lexer

#[derive(Debug, Clone)]
struct StrLit {
    prefix: String,
    quote: char,
    triple: bool,
    body: String,
    terminated: bool,
    /// The name when the whole body is one placeholder of a plain string.
    slot: Option<String>,
}

#[derive(Debug, Clone)]
enum Kind {
    Name(String),
    Number(String),
    Str(StrLit),
    Op(String),
    /// A bare placeholder; its occurrence is already recorded.
    Slot,
    Newline,
}

#[derive(Debug, Clone)]
struct Token {
    kind: Kind,
    line: usize,
    column: usize,
    /// Bracket depth: before an opening bracket, after a closing one.
    depth: usize,
    /// Index of the innermost enclosing opening bracket.
    open: Option<usize>,
}

struct Comment {
    line: usize,
    /// The text after `#`.
    text: String,
}

#[derive(Default)]
struct Lexed {
    tokens: Vec<Token>,
    comments: Vec<Comment>,
    /// Comment and bare occurrences (quoted ones follow from the tokens).
    occurrences: Vec<Occurrence>,
    violations: Vec<Violation>,
}

struct Cursor {
    chars: Vec<char>,
    pos: usize,
    line: usize,
    column: usize,
}

impl Cursor {
    fn peek(&self, ahead: usize) -> Option<char> {
        self.chars.get(self.pos + ahead).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let character = self.chars.get(self.pos).copied()?;
        self.pos += 1;
        if character == '\n' {
            self.line += 1;
            self.column = 1;
        } else {
            self.column += 1;
        }
        Some(character)
    }
}

const KEYWORDS: [&str; 32] = [
    "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del", "elif",
    "else", "except", "finally", "for", "from", "global", "if", "import", "in", "is", "lambda",
    "nonlocal", "not", "or", "pass", "raise", "return", "try", "while", "with", "yield",
];

const COMPOUND: [&str; 12] = [
    "if", "elif", "else", "while", "for", "with", "try", "except", "finally", "def", "class",
    "async",
];

const THREE_CHAR_OPS: [&str; 5] = ["**=", "//=", ">>=", "<<=", "..."];
const TWO_CHAR_OPS: [&str; 19] = [
    "==", "!=", "<=", ">=", "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=", "@=", "**", "//", ">>",
    "<<", "->", ":=",
];
const AUGMENTED: [&str; 13] = [
    "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=", "@=", "**=", "//=", ">>=", "<<=",
];

fn is_keyword(name: &str) -> bool {
    KEYWORDS.contains(&name)
}

fn is_ident_start(character: char) -> bool {
    character.is_alphabetic() || character == '_'
}

fn is_ident_continue(character: char) -> bool {
    character.is_alphanumeric() || character == '_'
}

fn is_string_prefix(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "r" | "u" | "b" | "f" | "br" | "rb" | "fr" | "rf"
    )
}

/// The placeholder starting at `chars[start]` (which must be `[`): a name
/// `A-Z` then `[A-Z0-9_ ]*`, closed by `]`. Returns the name and the number of
/// characters it spans.
fn placeholder_at(chars: &[char], start: usize) -> Option<(String, usize)> {
    if chars.get(start) != Some(&'[') {
        return None;
    }
    let first = *chars.get(start + 1)?;
    if !first.is_ascii_uppercase() {
        return None;
    }
    let mut end = start + 2;
    while let Some(&character) = chars.get(end) {
        if character.is_ascii_uppercase()
            || character.is_ascii_digit()
            || character == '_'
            || character == ' '
        {
            end += 1;
        } else {
            break;
        }
    }
    if chars.get(end) != Some(&']') {
        return None;
    }
    let name: String = chars[start + 1..end].iter().collect();
    Some((name, end + 1 - start))
}

/// Every placeholder in `chars`: `(offset, name, length)`.
fn placeholders_in(chars: &[char]) -> Vec<(usize, String, usize)> {
    let mut found = Vec::new();
    let mut at = 0;
    while at < chars.len() {
        if chars[at] == '[' {
            if let Some((name, length)) = placeholder_at(chars, at) {
                found.push((at, name, length));
                at += length;
                continue;
            }
        }
        at += 1;
    }
    found
}

fn lex(text: &str) -> Lexed {
    let mut lexer = Lexer {
        cursor: Cursor {
            chars: text.chars().collect(),
            pos: 0,
            line: 1,
            column: 1,
        },
        out: Lexed::default(),
        open: Vec::new(),
        value_end: false,
    };
    while let Some(character) = lexer.cursor.peek(0) {
        lexer.step(character);
    }
    lexer.out
}

struct Lexer {
    cursor: Cursor,
    out: Lexed,
    /// Indices of the currently open brackets.
    open: Vec<usize>,
    /// Whether the last significant token ends a value (so `[` indexes it).
    value_end: bool,
}

impl Lexer {
    fn push(&mut self, kind: Kind, line: usize, column: usize) {
        let depth = self.open.len();
        let open = self.open.last().copied();
        self.out.tokens.push(Token {
            kind,
            line,
            column,
            depth,
            open,
        });
    }

    fn step(&mut self, character: char) {
        let (line, column) = (self.cursor.line, self.cursor.column);
        match character {
            '\n' => {
                self.cursor.bump();
                self.push(Kind::Newline, line, column);
                if self.open.is_empty() {
                    self.value_end = false;
                }
            }
            ' ' | '\t' | '\r' | '\u{c}' => {
                self.cursor.bump();
            }
            '\\' if self.cursor.peek(1) == Some('\n') => {
                self.cursor.bump();
                self.cursor.bump();
            }
            '#' => self.comment(),
            '"' | '\'' => self.string(String::new(), line, column),
            '[' => self.open_bracket_or_slot(line, column),
            '(' | '{' => self.open_bracket(character, line, column),
            ')' | ']' | '}' => self.close_bracket(character, line, column),
            _ if is_ident_start(character) => self.name(line, column),
            _ if character.is_ascii_digit()
                || (character == '.'
                    && self
                        .cursor
                        .peek(1)
                        .is_some_and(|next| next.is_ascii_digit())) =>
            {
                self.number(line, column);
            }
            _ => self.operator(line, column),
        }
    }

    fn comment(&mut self) {
        let (line, column) = (self.cursor.line, self.cursor.column);
        self.cursor.bump();
        let mut text: Vec<char> = Vec::new();
        while let Some(character) = self.cursor.peek(0) {
            if character == '\n' {
                break;
            }
            text.push(character);
            self.cursor.bump();
        }
        for (offset, name, _) in placeholders_in(&text) {
            self.out.occurrences.push(Occurrence {
                name,
                context: Context::Comment,
                quote: None,
                line,
                column: column + 1 + offset,
            });
        }
        self.out.comments.push(Comment {
            line,
            text: text.into_iter().collect(),
        });
    }

    fn name(&mut self, line: usize, column: usize) {
        let mut end = self.cursor.pos;
        while self
            .cursor
            .chars
            .get(end)
            .is_some_and(|character| is_ident_continue(*character))
        {
            end += 1;
        }
        let name: String = self.cursor.chars[self.cursor.pos..end].iter().collect();
        let quoted = matches!(self.cursor.chars.get(end), Some('"' | '\''));
        if quoted && is_string_prefix(&name) {
            for _ in 0..name.chars().count() {
                self.cursor.bump();
            }
            self.string(name, line, column);
            return;
        }
        for _ in 0..name.chars().count() {
            self.cursor.bump();
        }
        self.value_end = !is_keyword(&name);
        self.push(Kind::Name(name), line, column);
    }

    fn number(&mut self, line: usize, column: usize) {
        let mut text = String::new();
        let hex =
            self.cursor.peek(0) == Some('0') && matches!(self.cursor.peek(1), Some('x' | 'X'));
        while let Some(character) = self.cursor.peek(0) {
            if character.is_ascii_alphanumeric() || character == '_' || character == '.' {
                text.push(character);
                self.cursor.bump();
                if !hex
                    && (character == 'e' || character == 'E')
                    && matches!(self.cursor.peek(0), Some('+' | '-'))
                {
                    text.extend(self.cursor.bump());
                }
            } else {
                break;
            }
        }
        self.value_end = true;
        self.push(Kind::Number(text), line, column);
    }

    fn operator(&mut self, line: usize, column: usize) {
        let rest: String = self.cursor.chars[self.cursor.pos..]
            .iter()
            .take(3)
            .collect();
        let text = THREE_CHAR_OPS
            .iter()
            .chain(TWO_CHAR_OPS.iter())
            .find(|operator| rest.starts_with(**operator))
            .map_or_else(
                || rest.chars().next().unwrap_or(' ').to_string(),
                |operator| (*operator).to_string(),
            );
        for _ in 0..text.chars().count() {
            self.cursor.bump();
        }
        self.value_end = false;
        self.push(Kind::Op(text), line, column);
    }

    fn open_bracket(&mut self, character: char, line: usize, column: usize) {
        self.cursor.bump();
        let index = self.out.tokens.len();
        self.push(Kind::Op(character.to_string()), line, column);
        self.open.push(index);
        self.value_end = false;
    }

    fn close_bracket(&mut self, character: char, line: usize, column: usize) {
        self.cursor.bump();
        self.open.pop();
        self.push(Kind::Op(character.to_string()), line, column);
        self.value_end = true;
    }

    /// A `[` is a bare placeholder when it spells one and does not index the
    /// value before it; otherwise it opens a bracket.
    fn open_bracket_or_slot(&mut self, line: usize, column: usize) {
        if !self.value_end {
            if let Some((name, length)) = placeholder_at(&self.cursor.chars, self.cursor.pos) {
                for _ in 0..length {
                    self.cursor.bump();
                }
                self.out.occurrences.push(Occurrence {
                    name,
                    context: Context::Bare,
                    quote: None,
                    line,
                    column,
                });
                self.push(Kind::Slot, line, column);
                self.value_end = true;
                return;
            }
        }
        self.open_bracket('[', line, column);
    }

    fn string(&mut self, prefix: String, line: usize, column: usize) {
        let quote = self.cursor.bump().unwrap_or('"');
        let triple = self.cursor.peek(0) == Some(quote) && self.cursor.peek(1) == Some(quote);
        if triple {
            self.cursor.bump();
            self.cursor.bump();
        }
        let mut body = String::new();
        let mut positions: Vec<(usize, usize)> = Vec::new();
        let mut terminated = false;
        while let Some(character) = self.cursor.peek(0) {
            if character == '\\' {
                positions.push((self.cursor.line, self.cursor.column));
                body.extend(self.cursor.bump());
                if self.cursor.peek(0).is_some() {
                    positions.push((self.cursor.line, self.cursor.column));
                    body.extend(self.cursor.bump());
                }
            } else if character == quote
                && (!triple
                    || (self.cursor.peek(1) == Some(quote) && self.cursor.peek(2) == Some(quote)))
            {
                for _ in 0..if triple { 3 } else { 1 } {
                    self.cursor.bump();
                }
                terminated = true;
                break;
            } else if character == '\n' && !triple {
                break;
            } else {
                positions.push((self.cursor.line, self.cursor.column));
                body.extend(self.cursor.bump());
            }
        }
        let body_chars: Vec<char> = body.chars().collect();
        let mut slot = None;
        if terminated {
            let found = placeholders_in(&body_chars);
            if !triple && prefix.is_empty() {
                if let [(0, name, length)] = found.as_slice() {
                    if *length == body_chars.len() {
                        slot = Some(name.clone());
                    }
                }
            }
            if slot.is_none() {
                for (offset, name, _) in found {
                    let (at_line, at_column) = positions[offset];
                    self.out.violations.push(Violation {
                        code: "placeholder_in_string",
                        line: at_line,
                        column: at_column,
                        message: format!(
                            "placeholder [{name}] sits inside a string that is not exactly the \
                             placeholder; use a quoted slot (\"[{name}]\") or a comment"
                        ),
                    });
                }
            }
        } else {
            self.out.violations.push(Violation {
                code: "unterminated_string",
                line,
                column,
                message: "a string literal is not closed".to_string(),
            });
        }
        self.value_end = true;
        self.push(
            Kind::Str(StrLit {
                prefix,
                quote,
                triple,
                body,
                terminated,
                slot,
            }),
            line,
            column,
        );
    }
}

// ----------------------------------------------------------- classification

fn is_operator(token: &Token, text: &str) -> bool {
    matches!(&token.kind, Kind::Op(operator) if operator == text)
}

/// For every token, the index of the nearest token before it that is not a
/// line end (one pass, so a long run of blank lines is not rescanned).
fn previous_significant(tokens: &[Token]) -> Vec<Option<usize>> {
    let mut last = None;
    tokens
        .iter()
        .enumerate()
        .map(|(at, token)| {
            let previous = last;
            if !matches!(token.kind, Kind::Newline) {
                last = Some(at);
            }
            previous
        })
        .collect()
}

/// For every token, the index of the nearest token after it that is not a
/// line end.
fn next_significant(tokens: &[Token]) -> Vec<Option<usize>> {
    let mut following = vec![None; tokens.len()];
    let mut next = None;
    for (at, token) in tokens.iter().enumerate().rev() {
        following[at] = next;
        if !matches!(token.kind, Kind::Newline) {
            next = Some(at);
        }
    }
    following
}

/// Rule 4: a quoted occurrence is a sentinel when it is an operand of `==` or
/// `!=`, or a direct element of the bracketed right operand of `in`.
/// The line-end-free neighbours of every token (see `previous_significant`).
struct Neighbours {
    before: Vec<Option<usize>>,
    after: Vec<Option<usize>>,
    bounds: Vec<(usize, usize)>,
}

fn is_sentinel(tokens: &[Token], neighbours: &Neighbours, at: usize) -> bool {
    let (left, right) = neighbours.bounds[at];
    let comparison = |index: Option<usize>| {
        index.is_some_and(|index| {
            is_operator(&tokens[index], "==") || is_operator(&tokens[index], "!=")
        })
    };
    // Only neighbours that make the literal part of a larger expression
    // disqualify it. Opening brackets before it are expression boundaries;
    // after it, parentheses and brackets are call and subscript operators.
    let component = |index: Option<usize>, left_side: bool| {
        index.is_some_and(|index| match &tokens[index].kind {
            Kind::Str(_) => true,
            Kind::Op(op) => {
                matches!(
                    op.as_str(),
                    "+" | "-"
                        | "*"
                        | "**"
                        | "/"
                        | "//"
                        | "%"
                        | "@"
                        | "&"
                        | "|"
                        | "^"
                        | "~"
                        | "<<"
                        | ">>"
                        | "."
                ) || (!left_side && matches!(op.as_str(), "(" | "["))
            }
            _ => false,
        })
    };
    let before =
        if left > 0 && matches!(tokens[left - 1].kind, Kind::Newline) && tokens[left].depth == 0 {
            None
        } else {
            neighbours.before[left]
        };
    let after = if right + 1 < tokens.len()
        && matches!(tokens[right + 1].kind, Kind::Newline)
        && tokens[right].depth == 0
    {
        None
    } else {
        neighbours.after[right]
    };
    if (comparison(before) && !component(after, false))
        || (comparison(after) && !component(before, true))
    {
        return true;
    }
    tokens[left].open.is_some_and(|open| {
        let element_start = before == Some(open) || before.is_some_and(|i| is_operator(&tokens[i], ",") && tokens[i].open == Some(open));
        let element_end = after.is_some_and(|i| (is_operator(&tokens[i], ",") && tokens[i].open == Some(open)) || (matches!(&tokens[i].kind, Kind::Op(op) if matches!(op.as_str(), ")" | "]" | "}")) && tokens[i].depth == tokens[open].depth));
        element_start && element_end && neighbours.before[open]
            .is_some_and(|index| matches!(&tokens[index].kind, Kind::Name(name) if name == "in"))
    })
}

// Collapse parentheses containing exactly one literal. Each pair is visited once,
// even for deeply nested input, and calls and membership containers stay intact.
fn grouped_bounds(
    tokens: &[Token],
    before: &[Option<usize>],
    after: &[Option<usize>],
) -> Vec<(usize, usize)> {
    let mut bounds: Vec<_> = (0..tokens.len()).map(|i| (i, i)).collect();
    let mut representative: Vec<Option<usize>> = tokens
        .iter()
        .enumerate()
        .map(|(i, t)| matches!(t.kind, Kind::Str(_)).then_some(i))
        .collect();
    let mut stack = Vec::new();
    for (i, token) in tokens.iter().enumerate() {
        if matches!(&token.kind, Kind::Op(op) if matches!(op.as_str(), "(" | "[" | "{")) {
            stack.push(i);
        } else if matches!(&token.kind, Kind::Op(op) if matches!(op.as_str(), ")" | "]" | "}")) {
            let Some(open) = stack.pop() else { continue };
            if !is_operator(&tokens[open], "(")
                || !is_operator(token, ")")
                || before[open].is_some_and(|j| {
                    ends_value(&tokens[j]) || matches!(&tokens[j].kind, Kind::Name(n) if n == "in")
                })
            {
                continue;
            }
            if let Some(atom) = after[open].and_then(|j| representative[j]) {
                if after[bounds[atom].1] == Some(i) {
                    bounds[atom] = (open, i);
                    representative[open] = Some(atom);
                }
            }
        }
    }
    bounds
}

fn string_occurrences(tokens: &[Token]) -> Vec<Occurrence> {
    let before = previous_significant(tokens);
    let after = next_significant(tokens);
    let bounds = grouped_bounds(tokens, &before, &after);
    let neighbours = Neighbours {
        before,
        after,
        bounds,
    };
    tokens
        .iter()
        .enumerate()
        .filter_map(|(at, token)| match &token.kind {
            Kind::Str(literal) => literal.slot.as_ref().map(|name| Occurrence {
                name: name.clone(),
                context: if is_sentinel(tokens, &neighbours, at) {
                    Context::Sentinel
                } else {
                    Context::Quoted
                },
                quote: Some(literal.quote),
                line: token.line,
                column: token.column,
            }),
            _ => None,
        })
        .collect()
}

fn summarize(occurrences: &[Occurrence]) -> Vec<Slot> {
    let mut slots: BTreeMap<&str, Slot> = BTreeMap::new();
    for occurrence in occurrences {
        let slot = slots
            .entry(occurrence.name.as_str())
            .or_insert_with(|| Slot {
                name: occurrence.name.clone(),
                kind: SlotKind::String,
                code: 0,
                sentinel: 0,
                comment: 0,
            });
        match occurrence.context {
            Context::Quoted => slot.code += 1,
            Context::Bare => {
                slot.code += 1;
                slot.kind = SlotKind::Value;
            }
            Context::Comment => slot.comment += 1,
            Context::Sentinel => slot.sentinel += 1,
        }
    }
    slots
        .into_values()
        .map(|mut slot| {
            if is_secret_name(&slot.name) {
                slot.kind = SlotKind::Secret;
            }
            slot
        })
        .collect()
}

/// Rule 6 (typed bare slots) and the one-kind-per-name check.
fn slot_violations(occurrences: &[Occurrence]) -> Vec<Violation> {
    let mut violations = Vec::new();
    let mut first_code: BTreeMap<&str, Context> = BTreeMap::new();
    let mut reported: BTreeSet<&str> = BTreeSet::new();
    for occurrence in occurrences {
        match occurrence.context {
            Context::Bare => {
                if !TYPED_BARE_SLOTS.contains(&occurrence.name.as_str()) {
                    violations.push(Violation {
                        code: "untyped_bare_slot",
                        line: occurrence.line,
                        column: occurrence.column,
                        message: format!(
                            "bare slot [{}] has no entry in the type table (section 4.4.1)",
                            occurrence.name
                        ),
                    });
                }
            }
            Context::Quoted => {}
            Context::Comment | Context::Sentinel => continue,
        }
        let first = *first_code
            .entry(occurrence.name.as_str())
            .or_insert(occurrence.context);
        if first != occurrence.context && reported.insert(occurrence.name.as_str()) {
            violations.push(Violation {
                code: "mixed_slot_kinds",
                line: occurrence.line,
                column: occurrence.column,
                message: format!(
                    "placeholder [{}] is both a quoted and a bare slot in one template",
                    occurrence.name
                ),
            });
        }
    }
    violations
}

fn manual_edit(comment: &Comment, cells: &[Cell]) -> ManualEdit {
    let text = comment.text.strip_prefix(' ').unwrap_or(&comment.text);
    let shortened: String = text.chars().take(MANUAL_TEXT_CHARS).collect();
    ManualEdit {
        cell: cell_at(cells, comment.line).map(|cell| cell.number),
        line: comment.line,
        text: shortened.trim_end().to_string(),
    }
}

// ------------------------------------------------------------------ targets

/// The logical statements of the token stream: runs of token indices without
/// line ends, split at a line end or `;` outside brackets.
fn statements(tokens: &[Token]) -> Vec<Vec<usize>> {
    let mut statements = Vec::new();
    let mut current = Vec::new();
    for (at, token) in tokens.iter().enumerate() {
        let separator =
            token.depth == 0 && (matches!(token.kind, Kind::Newline) || is_operator(token, ";"));
        if separator {
            if !current.is_empty() {
                statements.push(std::mem::take(&mut current));
            }
        } else if !matches!(token.kind, Kind::Newline) {
            current.push(at);
        }
    }
    if !current.is_empty() {
        statements.push(current);
    }
    statements
}

/// Whether a token ends a value, so a bracket after it indexes or calls it.
fn ends_value(token: &Token) -> bool {
    match &token.kind {
        Kind::Name(name) => !is_keyword(name),
        Kind::Number(_) | Kind::Str(_) | Kind::Slot => true,
        Kind::Op(operator) => matches!(operator.as_str(), ")" | "]" | "}"),
        Kind::Newline => false,
    }
}

/// Counts the names a target group binds: plain names, also inside a tuple or
/// list target, but not an attribute, an indexed or a called base, nor what
/// sits inside the brackets of one.
fn bound_names(tokens: &[Token], group: &[usize], counts: &mut BTreeMap<String, usize>) {
    let mut skip: Vec<bool> = Vec::new();
    for (position, &at) in group.iter().enumerate() {
        let token = &tokens[at];
        let before = position.checked_sub(1).map(|index| &tokens[group[index]]);
        let after = group.get(position + 1).map(|index| &tokens[*index]);
        match &token.kind {
            Kind::Op(operator) if matches!(operator.as_str(), "(" | "[" | "{") => {
                let inherited = skip.last().copied().unwrap_or(false);
                skip.push(inherited || before.is_some_and(ends_value));
            }
            Kind::Op(operator) if matches!(operator.as_str(), ")" | "]" | "}") => {
                skip.pop();
            }
            Kind::Name(name) if !is_keyword(name) => {
                let attribute = before.is_some_and(|token| is_operator(token, "."));
                let base = after.is_some_and(|token| {
                    [".", "(", "["].iter().any(|text| is_operator(token, text))
                });
                if !attribute && !base && !skip.last().copied().unwrap_or(false) {
                    *counts.entry(name.clone()).or_insert(0) += 1;
                }
            }
            _ => {}
        }
    }
}

struct Candidate {
    name: String,
    line: usize,
    kind: LiteralKind,
    literal: String,
}

/// The literal kind of the value tokens `value`, if they are one literal that
/// `assign` may set (section 4.4.3, rule 3).
fn literal_of(tokens: &[Token], value: &[usize]) -> Option<(LiteralKind, String)> {
    let kinds: Vec<&Kind> = value.iter().map(|at| &tokens[*at].kind).collect();
    match kinds.as_slice() {
        [Kind::Str(literal)] => {
            let plain = literal.prefix.is_empty()
                && !literal.triple
                && literal.terminated
                && !literal.body.contains('\n');
            let chars: Vec<char> = literal.body.chars().collect();
            (plain && placeholders_in(&chars).is_empty()).then(|| {
                (
                    LiteralKind::String,
                    format!("{0}{1}{0}", literal.quote, literal.body),
                )
            })
        }
        [Kind::Name(name)] if name == "True" || name == "False" => {
            Some((LiteralKind::Bool, name.clone()))
        }
        [Kind::Number(text)] => number_literal(text, ""),
        [Kind::Op(sign), Kind::Number(text)] if sign == "-" => number_literal(text, "-"),
        _ => None,
    }
}

fn number_literal(text: &str, sign: &str) -> Option<(LiteralKind, String)> {
    let digits = text.replace('_', "");
    let hex = digits.starts_with("0x") || digits.starts_with("0X");
    let kind = if !digits.is_empty()
        && !text.starts_with('_')
        && !text.ends_with('_')
        && digits.bytes().all(|byte| byte.is_ascii_digit())
    {
        LiteralKind::Int
    } else if !hex && digits.parse::<f64>().is_ok_and(f64::is_finite) {
        LiteralKind::Float
    } else {
        return None;
    };
    Some((kind, format!("{sign}{text}")))
}

/// Section 4.4.3 rule 2: `N = <literal>` on one line, or `N = (`, the literal
/// on the next line and `)` alone on the third, starting in column 1.
fn candidate_of(tokens: &[Token], statement: &[usize]) -> Option<Candidate> {
    let first = &tokens[*statement.first()?];
    let Kind::Name(name) = &first.kind else {
        return None;
    };
    if first.column != 1 || is_keyword(name) || !is_operator(&tokens[*statement.get(1)?], "=") {
        return None;
    }
    let value = &statement[2..];
    let on_line = |range: &[usize], line: usize| range.iter().all(|at| tokens[*at].line == line);
    let literal_tokens: &[usize] = if value
        .first()
        .is_some_and(|at| is_operator(&tokens[*at], "("))
    {
        if value.len() < 3 {
            return None;
        }
        let (open, inner, close) = (value[0], &value[1..value.len() - 1], value[value.len() - 1]);
        let parenthesized = is_operator(&tokens[close], ")")
            && tokens[open].line == first.line
            && on_line(inner, first.line + 1)
            && tokens[close].line == first.line + 2;
        if !parenthesized {
            return None;
        }
        inner
    } else {
        if !on_line(value, first.line) {
            return None;
        }
        value
    };
    let (kind, literal) = literal_of(tokens, literal_tokens)?;
    Some(Candidate {
        name: name.clone(),
        line: first.line,
        kind,
        literal,
    })
}

/// Reads one statement: counts the names it assigns and, for a candidate
/// target, records it. A compound header (`if x:`) is skipped so that the
/// statement after its colon is read on its own.
fn read_statement(
    tokens: &[Token],
    statement: &[usize],
    counts: &mut BTreeMap<String, usize>,
    candidates: &mut Vec<Candidate>,
) {
    let mut start = 0;
    while let Some(&at) = statement.get(start) {
        let Kind::Name(name) = &tokens[at].kind else {
            break;
        };
        let soft_header = matches!(name.as_str(), "case" | "match")
            && statement.get(start + 1).is_some_and(|next| {
                !is_operator(&tokens[*next], ":")
                    && !is_operator(&tokens[*next], "=")
                    && !AUGMENTED.iter().any(|op| is_operator(&tokens[*next], op))
            })
            && statement[start + 1..]
                .iter()
                .take_while(|index| {
                    tokens[**index].depth != 0
                        || (!is_operator(&tokens[**index], "=")
                            && !AUGMENTED.iter().any(|op| is_operator(&tokens[**index], op)))
                })
                .any(|index| tokens[*index].depth == 0 && is_operator(&tokens[*index], ":"));
        if !COMPOUND.contains(&name.as_str()) && !soft_header {
            break;
        }
        let colon = statement[start..]
            .iter()
            .position(|index| tokens[*index].depth == 0 && is_operator(&tokens[*index], ":"));
        match colon {
            Some(offset) => start += offset + 1,
            None => return,
        }
    }
    let statement = &statement[start..];
    let operators: Vec<usize> = statement
        .iter()
        .enumerate()
        .filter(|(_, at)| {
            let token = &tokens[**at];
            token.depth == 0
                && matches!(&token.kind, Kind::Op(operator)
                    if operator == "=" || AUGMENTED.contains(&operator.as_str()))
        })
        .map(|(position, _)| position)
        .collect();
    // Every assignment operator is preceded by a target group: a chain
    // `a = b = 1` has two, and an augmented assignment its single one.
    let mut begin = 0;
    for &position in &operators {
        let mut group = &statement[begin..position];
        // `NAME: annotation = value`
        if let Some(colon) = group
            .iter()
            .position(|at| tokens[*at].depth == 0 && is_operator(&tokens[*at], ":"))
        {
            group = &group[..colon];
        }
        bound_names(tokens, group, counts);
        begin = position + 1;
    }
    if start == 0 && operators.len() == 1 {
        if let Some(candidate) = candidate_of(tokens, statement) {
            candidates.push(candidate);
        }
    }
}

/// The literal assignments `assign` may set: statements of the form
/// `N = <literal>` in a non-markdown cell, where `N` is assigned exactly once
/// in the whole template (section 4.4.3).
fn find_targets(tokens: &[Token], cells: &[Cell]) -> Vec<Target> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut candidates = Vec::new();
    for statement in statements(tokens) {
        read_statement(tokens, &statement, &mut counts, &mut candidates);
    }
    candidates
        .into_iter()
        .filter(|candidate| counts.get(&candidate.name) == Some(&1))
        .filter_map(|candidate| {
            let cell = cell_at(cells, candidate.line)?;
            (!cell.markdown).then_some(Target {
                name: candidate.name,
                cell: cell.number,
                line: candidate.line,
                kind: candidate.kind,
                literal: candidate.literal,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostile_text_never_panics_and_is_reported() {
        for text in [
            "",
            "\n\n\n",
            "\"",
            "'''",
            "f\"{",
            "[[[[[[[",
            "]]]]]]]",
            "# Cell",
            "# Cell 99999999999999999999999: x",
            "\\",
            "x = (",
            "\u{feff}",
            "\r\r\r",
            "0e",
            "1e+",
            ".5.5.5",
            "if:",
            "else: x = 1",
            "a = b = c",
            "(a, b), [c, d] = 1, 2",
        ] {
            let template = parse(text);
            assert!(template.occurrences.len() < 4, "{text:?}");
        }
    }

    #[test]
    fn deep_nesting_and_long_input_stay_linear() {
        let deep = format!("x = {}{}", "(".repeat(50_000), ")".repeat(50_000));
        assert!(parse(&deep).violations.iter().any(|v| v.code == "no_cells"));
        let newlines = format!("x = (\n{}\"[ROLE_ARN]\"\n)\n", "\n".repeat(100_000));
        assert_eq!(parse(&newlines).occurrences.len(), 1);
    }

    #[test]
    fn many_strings_after_a_long_run_of_blank_lines_stay_linear() {
        // A neighbour search that rescans the blank lines before the opening
        // bracket for every string would need billions of steps here.
        let text = format!(
            "{}f(\n{})\n",
            "\n".repeat(200_000),
            "\"[ROLE_ARN]\",\n".repeat(20_000)
        );
        assert_eq!(parse(&text).occurrences.len(), 20_000);
    }

    #[test]
    fn a_chain_and_a_tuple_count_every_bound_name() {
        let mut counts = BTreeMap::new();
        let lexed =
            lex("a = b = 1\n(c, d), [e, f] = 1, 2\ng.h = 1\ni[j] = 1\nk += 1\nl: int = 2\n");
        for statement in statements(&lexed.tokens) {
            read_statement(&lexed.tokens, &statement, &mut counts, &mut Vec::new());
        }
        let names: Vec<&str> = counts.keys().map(String::as_str).collect();
        assert_eq!(names, ["a", "b", "c", "d", "e", "f", "k", "l"]);
    }
}

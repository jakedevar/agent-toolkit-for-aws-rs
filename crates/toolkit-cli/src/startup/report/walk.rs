//! The five `HTMLParser` subclasses of the report validator, as handlers of
//! the tokenizer in [`super::html`]: section extraction, the decoded text runs
//! of the currency check, the data-cost anchors, the visible text and the
//! rendered fragment of the decision summary.

use std::collections::HashSet;

use super::html::{attr_lookup, has_attr, parse, Attr, Handler};
use super::rx::rx;
use crate::manifests::{is_py_space, py_strip};

/// Tags that run text together with their siblings.
const INLINE_TAGS: [&str; 25] = [
    "a", "abbr", "b", "bdi", "bdo", "cite", "code", "data", "dfn", "em", "i", "kbd", "mark", "q",
    "s", "samp", "small", "span", "strong", "sub", "sup", "time", "u", "var", "wbr",
];
/// Elements the browser never renders.
const INERT_TAGS: [&str; 3] = ["script", "style", "template"];
/// Elements without an end tag.
const VOID_TAGS: [&str; 14] = [
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];
const HEADING_TAGS: [&str; 6] = ["h1", "h2", "h3", "h4", "h5", "h6"];

fn is_inline(tag: &str) -> bool {
    INLINE_TAGS.contains(&tag)
}

fn is_inert(tag: &str) -> bool {
    INERT_TAGS.contains(&tag)
}

fn is_void(tag: &str) -> bool {
    VOID_TAGS.contains(&tag)
}

// ---------------------------------------------------------------------------
// _SectionScopeParser
// ---------------------------------------------------------------------------

struct SectionScope<'a> {
    target: &'a str,
    found: Option<String>,
    depth: usize,
    parts: Vec<String>,
}

impl Handler for SectionScope<'_> {
    fn start_tag(&mut self, tag: &str, attrs: &[Attr], text: &str) {
        if tag != "section" {
            if self.depth > 0 {
                self.parts.push(text.to_string());
            }
            return;
        }
        if self.depth > 0 {
            self.depth += 1;
            self.parts.push(text.to_string());
            return;
        }
        let matches_target =
            attr_lookup(attrs, "id").is_some_and(|value| value.as_deref() == Some(self.target));
        if matches_target {
            self.depth = 1;
            self.parts.clear();
        }
    }

    fn start_end_tag(&mut self, _tag: &str, _attrs: &[Attr], text: &str) {
        if self.depth > 0 {
            self.parts.push(text.to_string());
        }
    }

    fn end_tag(&mut self, tag: &str) {
        if tag != "section" || self.depth == 0 {
            if self.depth > 0 {
                self.parts.push(format!("</{tag}>"));
            }
            return;
        }
        self.depth -= 1;
        if self.depth == 0 {
            if self.found.is_none() {
                self.found = Some(self.parts.concat());
            }
        } else {
            self.parts.push("</section>".to_string());
        }
    }

    fn data(&mut self, data: &str) {
        if self.depth > 0 {
            // Re-escape the markup characters so the text survives a second
            // parse as text.
            self.parts.push(
                data.replace('&', "&amp;")
                    .replace('<', "&lt;")
                    .replace('>', "&gt;"),
            );
        }
    }

    fn comment(&mut self, data: &str) {
        if self.depth > 0 {
            self.parts.push(format!("<!--{data}-->"));
        }
    }
}

/// `_section_html`: the inner HTML of the first `<section id="...">`, found by
/// the tag structure, or `None` when that section is absent.
pub(super) fn section_html(html: &str, section_id: &str) -> Option<String> {
    let mut scope = SectionScope {
        target: section_id,
        found: None,
        depth: 0,
        parts: Vec::new(),
    };
    parse(html, &mut scope);
    scope.found
}

// ---------------------------------------------------------------------------
// _VisibleTextParser / _plain_text
// ---------------------------------------------------------------------------

struct HiddenFrame {
    tag: String,
    hidden: bool,
}

#[derive(Default)]
struct VisibleText {
    parts: Vec<String>,
    inert_depth: usize,
    stack: Vec<HiddenFrame>,
}

impl VisibleText {
    fn hidden(&self) -> bool {
        self.stack.last().is_some_and(|frame| frame.hidden)
    }
}

impl Handler for VisibleText {
    fn start_tag(&mut self, tag: &str, attrs: &[Attr], _text: &str) {
        if is_inert(tag) {
            self.inert_depth += 1;
            return;
        }
        if self.inert_depth > 0 {
            return;
        }
        let hidden = self.hidden() || has_attr(attrs, "hidden");
        if !is_inline(tag) {
            self.parts.push(" ".to_string());
        }
        if !is_void(tag) {
            self.stack.push(HiddenFrame {
                tag: tag.to_string(),
                hidden,
            });
        }
    }

    fn start_end_tag(&mut self, _tag: &str, _attrs: &[Attr], _text: &str) {}

    fn end_tag(&mut self, tag: &str) {
        if is_inert(tag) {
            self.inert_depth = self.inert_depth.saturating_sub(1);
            return;
        }
        if self.inert_depth > 0 {
            return;
        }
        if let Some(index) = self.stack.iter().rposition(|frame| frame.tag == tag) {
            self.stack.truncate(index);
        }
        if !is_inline(tag) {
            self.parts.push(" ".to_string());
        }
    }

    fn data(&mut self, data: &str) {
        if self.inert_depth == 0 && !self.hidden() {
            self.parts.push(data.to_string());
        }
    }
}

/// `_plain_text`: decoded visible text, dashes folded, whitespace collapsed,
/// stripped and lower-cased. Inert and hidden subtrees and comments are not
/// part of it.
pub(super) fn plain_text(fragment: &str) -> String {
    let mut visible = VisibleText::default();
    parse(fragment, &mut visible);
    let text = visible.parts.concat();
    let text = text.replace(['\u{2014}', '\u{2013}'], "-");
    let mut collapsed = String::with_capacity(text.len());
    let mut in_space = false;
    for c in text.chars() {
        if is_py_space(c) {
            if !in_space {
                collapsed.push(' ');
            }
            in_space = true;
        } else {
            collapsed.push(c);
            in_space = false;
        }
    }
    py_strip(&collapsed).to_lowercase()
}

// ---------------------------------------------------------------------------
// _DecodedTextRunParser
// ---------------------------------------------------------------------------

/// The state of one open `<table>`.
#[derive(Default)]
struct TableState {
    calc_col_index: Option<usize>,
    in_calc_cell: bool,
    cell_col: usize,
    pending_th_col: Option<usize>,
}

#[derive(Default)]
struct DecodedRuns {
    parts: Vec<String>,
    calc: Vec<bool>,
    inert_depth: usize,
    tables: Vec<TableState>,
    boundaries: Vec<usize>,
    length: usize,
}

impl DecodedRuns {
    fn emit(&mut self, text: &str, is_boundary: bool) {
        if text.is_empty() {
            return;
        }
        if is_boundary {
            self.boundaries.push(self.length);
        }
        let in_calc = self.tables.last().is_some_and(|table| table.in_calc_cell);
        self.length += text.len();
        self.parts.push(text.to_string());
        self.calc.push(in_calc);
    }
}

impl Handler for DecodedRuns {
    fn start_tag(&mut self, tag: &str, _attrs: &[Attr], _text: &str) {
        if is_inert(tag) {
            self.inert_depth += 1;
            return;
        }
        if self.inert_depth > 0 {
            return;
        }
        if tag == "table" {
            self.tables.push(TableState::default());
        } else if tag == "thead" || tag == "tr" {
            let starts_row = tag == "tr";
            if let Some(table) = self.tables.last_mut().filter(|_| starts_row) {
                table.cell_col = 0;
            }
        } else if (tag == "th" || tag == "td") && !self.tables.is_empty() {
            if let Some(table) = self.tables.last_mut() {
                let column = table.cell_col;
                if tag == "th" {
                    table.pending_th_col = Some(column);
                } else if table.calc_col_index == Some(column) {
                    table.in_calc_cell = true;
                }
            }
        } else if !is_inline(tag) {
            self.emit(" ", true);
        }
    }

    fn start_end_tag(&mut self, tag: &str, _attrs: &[Attr], _text: &str) {
        if !is_inline(tag) && !is_inert(tag) {
            self.emit(" ", true);
        }
    }

    fn end_tag(&mut self, tag: &str) {
        if is_inert(tag) {
            self.inert_depth = self.inert_depth.saturating_sub(1);
            return;
        }
        if self.inert_depth > 0 {
            return;
        }
        if tag == "th"
            && self
                .tables
                .last()
                .is_some_and(|t| t.pending_th_col.is_some())
        {
            if let Some(table) = self.tables.last_mut() {
                table.cell_col = table.pending_th_col.unwrap_or(0) + 1;
            }
        } else if tag == "td" && !self.tables.is_empty() {
            if let Some(table) = self.tables.last_mut() {
                table.in_calc_cell = false;
                table.cell_col += 1;
            }
        } else if tag == "table" {
            self.tables.pop();
        }
        if !is_inline(tag) {
            self.emit(" ", true);
        }
    }

    fn data(&mut self, data: &str) {
        if self.inert_depth > 0 {
            return;
        }
        if let Some(table) = self.tables.last_mut() {
            if let Some(column) = table.pending_th_col {
                if rx!(r"(?i)calculation|\bnotes\b").is_match(data) {
                    table.calc_col_index = Some(column);
                }
            }
        }
        self.emit(data, false);
    }
}

/// The rendered text of the report body with, per byte, whether it lies in a
/// "Calculation"/"Notes" cell, and the offsets of the block-level separators.
pub(super) struct DecodedText {
    pub(super) text: String,
    pub(super) calc_mask: Vec<bool>,
    pub(super) boundaries: Vec<usize>,
}

/// `_decoded_text_with_calc_mask` for an already scoped document.
pub(super) fn decoded_text(scope: &str) -> DecodedText {
    let mut runs = DecodedRuns::default();
    parse(scope, &mut runs);
    let mut calc_mask = Vec::with_capacity(runs.length);
    for (part, in_calc) in runs.parts.iter().zip(&runs.calc) {
        calc_mask.extend(std::iter::repeat_n(*in_calc, part.len()));
    }
    DecodedText {
        text: runs.parts.concat(),
        calc_mask,
        boundaries: runs.boundaries,
    }
}

// ---------------------------------------------------------------------------
// _CostAnchorParser
// ---------------------------------------------------------------------------

struct AnchorFrame {
    tag: String,
    inert: bool,
    hidden: bool,
    /// The `data-cost-key` (lower-cased) and the text collected so far.
    anchor: Option<(String, Vec<String>)>,
}

#[derive(Default)]
struct CostAnchors {
    results: Vec<(String, String)>,
    stack: Vec<AnchorFrame>,
}

impl CostAnchors {
    fn in_skip(&self) -> bool {
        self.stack
            .last()
            .is_some_and(|frame| frame.inert || frame.hidden)
    }
}

/// The non-empty value of an attribute (`if key:`).
fn truthy_attr<'a>(attrs: &'a [Attr], name: &str) -> Option<&'a str> {
    attr_lookup(attrs, name)
        .and_then(Option::as_deref)
        .filter(|value| !value.is_empty())
}

impl Handler for CostAnchors {
    fn start_tag(&mut self, tag: &str, attrs: &[Attr], _text: &str) {
        let (parent_inert, parent_hidden) = self
            .stack
            .last()
            .map_or((false, false), |frame| (frame.inert, frame.hidden));
        let inert = parent_inert || is_inert(tag);
        let hidden = parent_hidden || has_attr(attrs, "hidden");
        let anchor = match truthy_attr(attrs, "data-cost-key") {
            Some(key) if !inert && !hidden => Some((key.to_lowercase(), Vec::new())),
            _ => None,
        };
        if !is_void(tag) {
            self.stack.push(AnchorFrame {
                tag: tag.to_string(),
                inert,
                hidden,
                anchor,
            });
        }
    }

    fn start_end_tag(&mut self, tag: &str, attrs: &[Attr], _text: &str) {
        if is_inert(tag) || self.in_skip() {
            return;
        }
        let parent_hidden = self.stack.last().is_some_and(|frame| frame.hidden);
        if let Some(key) = truthy_attr(attrs, "data-cost-key") {
            if !parent_hidden && !has_attr(attrs, "hidden") {
                self.results.push((key.to_lowercase(), String::new()));
            }
        }
    }

    fn end_tag(&mut self, tag: &str) {
        if is_void(tag) {
            return;
        }
        let Some(index) = self.stack.iter().rposition(|frame| frame.tag == tag) else {
            return;
        };
        let popped = self.stack.split_off(index);
        for frame in popped.into_iter().rev() {
            if let Some((key, parts)) = frame.anchor {
                self.results.push((key, parts.concat()));
            }
        }
    }

    fn data(&mut self, data: &str) {
        if self.in_skip() {
            return;
        }
        for frame in &mut self.stack {
            if let Some((_, parts)) = frame.anchor.as_mut() {
                parts.push(data.to_string());
            }
        }
    }
}

/// `_cost_anchor_matches`: every `(data-cost-key, rendered text)` pair outside
/// comments and non-rendered markup, nested anchors included.
pub(super) fn cost_anchor_matches(html: &str) -> Vec<(String, String)> {
    let mut anchors = CostAnchors::default();
    parse(html, &mut anchors);
    anchors.results
}

// ---------------------------------------------------------------------------
// _RenderedFragmentParser
// ---------------------------------------------------------------------------

/// What a run of rendered text is: a heading, a list item or other prose.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Kind {
    Heading,
    Item,
    Text,
}

/// The rendered view of a fragment, in document order.
#[derive(Default)]
pub(super) struct Rendered {
    pub(super) entries: Vec<(Kind, String)>,
    pub(super) class_tokens: HashSet<String>,
    inert_depth: usize,
    stack: Vec<HiddenFrame>,
    li_depth: usize,
    heading_depth: usize,
    buffer: Vec<String>,
}

impl Rendered {
    fn hidden(&self) -> bool {
        self.stack.last().is_some_and(|frame| frame.hidden)
    }

    /// `_pop_to`: pops through the nearest open `tag`; whether any popped
    /// element was hidden.
    fn pop_to(&mut self, tag: &str) -> bool {
        let Some(index) = self.stack.iter().rposition(|frame| frame.tag == tag) else {
            return false;
        };
        self.stack.split_off(index).iter().any(|frame| frame.hidden)
    }

    fn flush(&mut self) {
        let text = plain_text(&self.buffer.join(" "));
        self.buffer.clear();
        if text.is_empty() {
            return;
        }
        let kind = if self.li_depth > 0 {
            Kind::Item
        } else if self.heading_depth > 0 {
            Kind::Heading
        } else {
            Kind::Text
        };
        self.entries.push((kind, text));
    }

    fn record_classes(&mut self, attrs: &[Attr]) {
        for (name, value) in attrs {
            if name == "class" {
                if let Some(value) = value {
                    self.class_tokens.extend(
                        value
                            .split(is_py_space)
                            .filter(|token| !token.is_empty())
                            .map(str::to_string),
                    );
                }
            }
        }
    }
}

impl Handler for Rendered {
    fn start_tag(&mut self, tag: &str, attrs: &[Attr], _text: &str) {
        if is_inert(tag) {
            self.inert_depth += 1;
            return;
        }
        if self.inert_depth > 0 {
            return;
        }
        let hidden = self.hidden() || has_attr(attrs, "hidden");
        if !is_void(tag) {
            self.stack.push(HiddenFrame {
                tag: tag.to_string(),
                hidden,
            });
        }
        if hidden {
            return;
        }
        self.record_classes(attrs);
        if !is_inline(tag) {
            self.flush();
        }
        if tag == "li" {
            self.li_depth += 1;
        } else if HEADING_TAGS.contains(&tag) {
            self.heading_depth += 1;
        }
    }

    fn start_end_tag(&mut self, tag: &str, attrs: &[Attr], _text: &str) {
        if is_inert(tag) || self.inert_depth > 0 || self.hidden() || has_attr(attrs, "hidden") {
            return;
        }
        self.record_classes(attrs);
        if !is_inline(tag) {
            self.flush();
        }
    }

    fn end_tag(&mut self, tag: &str) {
        if is_inert(tag) {
            self.inert_depth = self.inert_depth.saturating_sub(1);
            return;
        }
        if self.inert_depth > 0 {
            return;
        }
        let popped_hidden = self.pop_to(tag);
        if popped_hidden || self.hidden() {
            return;
        }
        if !is_inline(tag) {
            self.flush();
        }
        if tag == "li" && self.li_depth > 0 {
            self.li_depth -= 1;
        } else if HEADING_TAGS.contains(&tag) && self.heading_depth > 0 {
            self.heading_depth -= 1;
        }
    }

    fn data(&mut self, data: &str) {
        if self.inert_depth == 0 && !self.hidden() {
            self.buffer.push(data.to_string());
        }
    }
}

/// `_rendered_fragment`.
pub(super) fn rendered_fragment(fragment: &str) -> Rendered {
    let mut rendered = Rendered::default();
    parse(fragment, &mut rendered);
    rendered.flush();
    rendered
}

//! The `HTMLParser` subclasses of `validate-heroku-migration-report.py` that
//! the full validator of [`super::super`] does not already provide, as
//! handlers of the shared tokenizer: rendered `<section>` structure, the
//! decoded text runs of the currency check, the rendered start tags, the
//! opportunity-row scan and the accessibility scan.

use std::collections::HashMap;

use super::super::html::{attr_lookup, has_attr, parse, Attr, Handler};
use super::super::rx::rx;
use crate::manifests::{is_py_space, py_strip};

/// Elements the browser never renders.
const INERT_TAGS: [&str; 3] = ["script", "style", "template"];
/// Elements without an end tag.
const VOID_TAGS: [&str; 14] = [
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];
/// Tags that run text together with their siblings.
const INLINE_TAGS: [&str; 25] = [
    "a", "abbr", "b", "bdi", "bdo", "cite", "code", "data", "dfn", "em", "i", "kbd", "mark", "q",
    "s", "samp", "small", "span", "strong", "sub", "sup", "time", "u", "var", "wbr",
];

fn is_inert(tag: &str) -> bool {
    INERT_TAGS.contains(&tag)
}

fn is_void(tag: &str) -> bool {
    VOID_TAGS.contains(&tag)
}

fn is_inline(tag: &str) -> bool {
    INLINE_TAGS.contains(&tag)
}

/// `bool(dict(attrs).get(name))`: the attribute has a non-empty value.
fn attr_text<'a>(attrs: &'a [Attr], name: &str) -> Option<&'a str> {
    attr_lookup(attrs, name)
        .and_then(Option::as_deref)
        .filter(|value| !value.is_empty())
}

// ---------------------------------------------------------------------------
// _SectionParser
// ---------------------------------------------------------------------------

struct OpenSection {
    id: String,
    parts: Vec<String>,
    depth: usize,
}

/// Rendered `<section>` structure: per-id counts and inner-HTML fragments,
/// without comments and inert (`script`/`style`/`template`) subtrees.
///
/// Python reads the document with `convert_charrefs=False` and re-emits the raw
/// text and references. The shared tokenizer decodes text, so the fragments
/// re-escape `&`, `<` and `>` instead; every later parser decodes them again,
/// which yields the same rendered text.
#[derive(Default)]
pub(super) struct Sections {
    counts: HashMap<String, usize>,
    fragments: HashMap<String, Vec<String>>,
    inert_depth: usize,
    open: Vec<OpenSection>,
    depth: usize,
}

impl Sections {
    fn emit(&mut self, markup: &str) {
        for section in &mut self.open {
            section.parts.push(markup.to_string());
        }
    }

    fn count(&mut self, id: &str) {
        *self.counts.entry(id.to_string()).or_insert(0) += 1;
    }

    /// `_section_counts(html).get(id, 0)`.
    pub(super) fn count_of(&self, id: &str) -> usize {
        self.counts.get(id).copied().unwrap_or(0)
    }

    /// `_section_html`: the first closed fragment of the section, `None` when
    /// it is absent, inert, commented out or never closed.
    pub(super) fn html(&self, id: &str) -> Option<&str> {
        self.fragments
            .get(id)
            .and_then(|fragments| fragments.first())
            .map(String::as_str)
    }
}

impl Handler for Sections {
    fn start_tag(&mut self, tag: &str, attrs: &[Attr], text: &str) {
        if is_inert(tag) {
            self.inert_depth += 1;
            return;
        }
        if self.inert_depth > 0 {
            return;
        }
        if tag == "section" {
            // Nested section attributes stay in the containing fragment so a
            // second parser retains cost anchors and hidden ancestry.
            self.emit(text);
            self.depth += 1;
            if let Some(id) = attr_text(attrs, "id") {
                self.count(id);
                self.fragments.entry(id.to_string()).or_default();
                self.open.push(OpenSection {
                    id: id.to_string(),
                    parts: Vec::new(),
                    depth: self.depth,
                });
            }
            return;
        }
        self.emit(text);
    }

    fn start_end_tag(&mut self, tag: &str, attrs: &[Attr], text: &str) {
        if is_inert(tag) || self.inert_depth > 0 {
            return;
        }
        if tag == "section" {
            self.emit(text);
            if let Some(id) = attr_text(attrs, "id") {
                self.count(id);
                self.fragments
                    .entry(id.to_string())
                    .or_default()
                    .push(String::new());
            }
            return;
        }
        self.emit(text);
    }

    fn end_tag(&mut self, tag: &str) {
        if is_inert(tag) {
            self.inert_depth = self.inert_depth.saturating_sub(1);
            return;
        }
        if self.inert_depth > 0 {
            return;
        }
        if tag == "section" {
            while self
                .open
                .last()
                .is_some_and(|section| section.depth >= self.depth)
            {
                if let Some(done) = self.open.pop() {
                    self.fragments
                        .entry(done.id)
                        .or_default()
                        .push(done.parts.concat());
                }
            }
            self.depth = self.depth.saturating_sub(1);
            self.emit("</section>");
            return;
        }
        self.emit(&format!("</{tag}>"));
    }

    fn data(&mut self, data: &str) {
        if self.inert_depth == 0 {
            let escaped = data
                .replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;");
            self.emit(&escaped);
        }
    }
}

/// `_parse_sections`.
pub(super) fn parse_sections(html: &str) -> Sections {
    let mut sections = Sections::default();
    parse(html, &mut sections);
    sections
}

// ---------------------------------------------------------------------------
// _DecodedTextParser
// ---------------------------------------------------------------------------

struct HiddenFrame {
    tag: String,
    hidden: bool,
}

/// The rendered text of a document as the browser presents it, with the
/// offsets of every block-level separator.
#[derive(Default)]
pub(super) struct DecodedText {
    parts: Vec<String>,
    inert_depth: usize,
    stack: Vec<HiddenFrame>,
    boundaries: Vec<usize>,
    length: usize,
}

impl DecodedText {
    fn append(&mut self, text: &str, is_boundary: bool) {
        if text.is_empty() {
            return;
        }
        if is_boundary {
            self.boundaries.push(self.length);
        }
        self.length += text.len();
        self.parts.push(text.to_string());
    }

    fn hidden(&self) -> bool {
        self.stack.last().is_some_and(|frame| frame.hidden)
    }

    /// The rendered text.
    pub(super) fn text(&self) -> String {
        self.parts.concat()
    }

    /// Byte offsets of the block-level separators in [`DecodedText::text`].
    pub(super) fn boundaries(&self) -> &[usize] {
        &self.boundaries
    }
}

impl Handler for DecodedText {
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
        if !is_inline(tag) {
            self.append(" ", true);
        }
    }

    fn start_end_tag(&mut self, tag: &str, attrs: &[Attr], _text: &str) {
        if is_inert(tag) || self.inert_depth > 0 || self.hidden() || has_attr(attrs, "hidden") {
            return;
        }
        if !is_inline(tag) {
            self.append(" ", true);
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
        if let Some(index) = self.stack.iter().rposition(|frame| frame.tag == tag) {
            self.stack.truncate(index);
        }
        if !is_inline(tag) {
            self.append(" ", true);
        }
    }

    fn data(&mut self, data: &str) {
        if self.inert_depth == 0 && !self.hidden() {
            self.append(data, false);
        }
    }
}

/// `_DecodedTextParser` over an already scoped document.
pub(super) fn decoded_text(scope: &str) -> DecodedText {
    let mut decoded = DecodedText::default();
    parse(scope, &mut decoded);
    decoded
}

// ---------------------------------------------------------------------------
// _TagAttrCollector
// ---------------------------------------------------------------------------

/// A rendered start tag with its attributes.
pub(super) struct RenderedTag {
    pub(super) name: String,
    attrs: Vec<Attr>,
}

impl RenderedTag {
    /// `dict(attrs).get(name)`: `None` when the attribute is absent, the
    /// inner `None` for an attribute written without a value.
    pub(super) fn attr(&self, name: &str) -> Option<&Option<String>> {
        attr_lookup(&self.attrs, name)
    }
}

/// Every rendered start tag: tags inside an inert subtree or below a
/// `hidden` element are skipped.
#[derive(Default)]
pub(super) struct Tags {
    found: Vec<RenderedTag>,
    inert_depth: usize,
    stack: Vec<HiddenFrame>,
}

impl Tags {
    fn hidden(&self) -> bool {
        self.stack.last().is_some_and(|frame| frame.hidden)
    }

    fn record(&mut self, tag: &str, attrs: &[Attr]) {
        self.found.push(RenderedTag {
            name: tag.to_string(),
            attrs: attrs.to_vec(),
        });
    }
}

impl Handler for Tags {
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
        if !hidden {
            self.record(tag, attrs);
        }
    }

    fn start_end_tag(&mut self, tag: &str, attrs: &[Attr], _text: &str) {
        if is_inert(tag) || self.inert_depth > 0 || self.hidden() {
            return;
        }
        if !has_attr(attrs, "hidden") {
            self.record(tag, attrs);
        }
    }

    fn end_tag(&mut self, tag: &str) {
        if is_inert(tag) && self.inert_depth > 0 {
            self.inert_depth -= 1;
            return;
        }
        if self.inert_depth > 0 {
            return;
        }
        if let Some(index) = self.stack.iter().rposition(|frame| frame.tag == tag) {
            self.stack.truncate(index);
        }
    }

    fn data(&mut self, _data: &str) {}
}

/// `_collect_tags`.
pub(super) fn collect_tags(html: &str) -> Vec<RenderedTag> {
    let mut tags = Tags::default();
    parse(html, &mut tags);
    tags.found
}

/// `_html_lang_declared`: the first rendered `<html>` declares a language
/// such as `en` or `pt-BR`.
pub(super) fn html_lang_declared(html: &str) -> bool {
    for tag in collect_tags(html) {
        if tag.name != "html" {
            continue;
        }
        return tag
            .attr("lang")
            .and_then(Option::as_deref)
            .is_some_and(|lang| rx!(r"(?i)\A[a-z]{2}(?:-[A-Za-z0-9]+)?\z").is_match(lang));
    }
    false
}

/// `_class_tokens`: every `class` token of every rendered tag.
pub(super) fn class_tokens(html_fragment: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    for tag in collect_tags(html_fragment) {
        if let Some(classes) = tag.attr("class").and_then(Option::as_deref) {
            tokens.extend(
                classes
                    .split(is_py_space)
                    .filter(|token| !token.is_empty())
                    .map(str::to_string),
            );
        }
    }
    tokens
}

// ---------------------------------------------------------------------------
// _OpportunityRowParser
// ---------------------------------------------------------------------------

/// Whether a `<table>` holds a `<td>` (not `<th>`) with rendered text.
#[derive(Default)]
struct OpportunityRows {
    found: bool,
    table_depth: usize,
    tr_depth: usize,
    td_open: bool,
    td_has_text: bool,
    inert_depth: usize,
}

impl OpportunityRows {
    /// Finalizes the open `<td>`, as a real parser does when its end tag is
    /// omitted before the next cell, the row or the table closes.
    fn close_cell(&mut self) {
        if self.td_open && self.td_has_text {
            self.found = true;
        }
        self.td_open = false;
        self.td_has_text = false;
    }
}

impl Handler for OpportunityRows {
    fn start_tag(&mut self, tag: &str, _attrs: &[Attr], _text: &str) {
        if is_inert(tag) {
            self.inert_depth += 1;
            return;
        }
        if self.inert_depth > 0 {
            return;
        }
        if tag == "table" {
            self.table_depth += 1;
        } else if tag == "tr" && self.table_depth > 0 {
            self.close_cell();
            self.tr_depth += 1;
        } else if (tag == "td" || tag == "th") && self.tr_depth > 0 {
            self.close_cell();
            if tag == "td" {
                self.td_open = true;
            }
        }
    }

    fn start_end_tag(&mut self, tag: &str, _attrs: &[Attr], _text: &str) {
        if is_inert(tag) || self.inert_depth > 0 {
            return;
        }
        if (tag == "td" || tag == "th") && self.tr_depth > 0 {
            self.close_cell();
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
        match tag {
            "td" | "th" => self.close_cell(),
            "tr" => {
                self.close_cell();
                self.tr_depth = self.tr_depth.saturating_sub(1);
            }
            "table" => {
                self.close_cell();
                self.table_depth = self.table_depth.saturating_sub(1);
            }
            _ => {}
        }
    }

    fn data(&mut self, data: &str) {
        if self.inert_depth == 0 && self.td_open && !py_strip(data).is_empty() {
            self.td_has_text = true;
        }
    }
}

/// `_has_populated_opportunity_row`.
pub(super) fn has_populated_opportunity_row(body: &str) -> bool {
    let mut rows = OpportunityRows::default();
    parse(body, &mut rows);
    rows.found
}

// ---------------------------------------------------------------------------
// _AccessibilityParser
// ---------------------------------------------------------------------------

struct FigureFrame {
    index: usize,
    has_aria_label: bool,
    has_figcaption: bool,
}

/// `<th scope>` and `<figure aria-label>` + `<figcaption>` findings.
#[derive(Default)]
struct Accessibility {
    /// 1-based index of every table with a header cell lacking a scope.
    table_issues: Vec<usize>,
    /// `(index, has_aria_label, has_figcaption)` of each closed figure.
    figure_issues: Vec<(usize, bool, bool)>,
    table_index: usize,
    figure_index: usize,
    table_depth: usize,
    table_bad_at: HashMap<usize, bool>,
    figures: Vec<FigureFrame>,
    inert_depth: usize,
}

impl Handler for Accessibility {
    fn start_tag(&mut self, tag: &str, attrs: &[Attr], _text: &str) {
        if is_inert(tag) {
            self.inert_depth += 1;
            return;
        }
        if self.inert_depth > 0 {
            return;
        }
        if tag == "table" {
            self.table_depth += 1;
            if self.table_depth == 1 {
                self.table_index += 1;
                self.table_bad_at.insert(self.table_index, false);
            }
        } else if tag == "th" && self.table_depth > 0 {
            let scoped = attr_text(attrs, "scope")
                .is_some_and(|scope| matches!(scope.to_lowercase().as_str(), "col" | "row"));
            if !scoped {
                self.table_bad_at.insert(self.table_index, true);
            }
        } else if tag == "figure" {
            self.figure_index += 1;
            self.figures.push(FigureFrame {
                index: self.figure_index,
                has_aria_label: attr_text(attrs, "aria-label")
                    .is_some_and(|label| !py_strip(label).is_empty()),
                has_figcaption: false,
            });
        } else if tag == "figcaption" {
            if let Some(figure) = self.figures.last_mut() {
                figure.has_figcaption = true;
            }
        }
    }

    // A self-closed tag never opens a scope for descendants.
    fn start_end_tag(&mut self, _tag: &str, _attrs: &[Attr], _text: &str) {}

    fn end_tag(&mut self, tag: &str) {
        if is_inert(tag) {
            self.inert_depth = self.inert_depth.saturating_sub(1);
            return;
        }
        if self.inert_depth > 0 {
            return;
        }
        if tag == "table" && self.table_depth > 0 {
            self.table_depth -= 1;
            if self.table_depth == 0
                && self
                    .table_bad_at
                    .get(&self.table_index)
                    .copied()
                    .unwrap_or(false)
            {
                self.table_issues.push(self.table_index);
            }
        } else if tag == "figure" {
            if let Some(figure) = self.figures.pop() {
                self.figure_issues.push((
                    figure.index,
                    figure.has_aria_label,
                    figure.has_figcaption,
                ));
            }
        }
    }

    fn data(&mut self, _data: &str) {}
}

/// The accessibility findings of `<style>`-free markup, in report order.
pub(super) fn accessibility_findings(body: &str) -> (Vec<usize>, Vec<(usize, bool, bool)>) {
    let mut scan = Accessibility::default();
    parse(body, &mut scan);
    (scan.table_issues, scan.figure_issues)
}

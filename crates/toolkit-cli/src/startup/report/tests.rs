//! Tests for `toolkit startup migration-report`: the HTML tokenizer and
//! `unescape`, the parser-based extractors, every validator on small
//! documents, the decision-core cases migrated from
//! `test_validate_migration_report_decision_core.py`, the shipped fixtures and
//! the command itself.

use std::ffi::OsString;
use std::fs;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use toolkit_core::Json;

use super::html::{parse, unescape, Attr, Handler};
use super::walk::{
    cost_anchor_matches, decoded_text, plain_text, rendered_fragment, section_html, Kind,
};
use super::{decision, figures, structure, style, validate_report, Artifacts, Mode, Options};
use crate::run;
use crate::startup::artifacts::pyjson::loads;

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "toolkit-report-{label}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create temp dir");
        Self(path)
    }
}

impl Deref for TempDir {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/aws-startup-advisor/fixtures")
}

fn reference_html() -> String {
    fs::read_to_string(fixtures().join("migration-report-reference.html")).expect("reference")
}

fn json_file(path: &Path) -> Json {
    loads(&fs::read_to_string(path).expect("read json")).expect("valid json")
}

fn reference_estimate() -> Json {
    json_file(&fixtures().join("estimation-infra-reference.json"))
}

fn reference_ai() -> Json {
    json_file(&fixtures().join("estimation-ai-reference.json"))
}

/// Sets the member at `path`, creating the last key when it is absent.
fn set(value: &mut Json, path: &[&str], new: Json) {
    let Some((first, rest)) = path.split_first() else {
        *value = new;
        return;
    };
    let Json::Object(members) = value else {
        panic!("not an object");
    };
    if let Some((_, inner)) = members.iter_mut().find(|(key, _)| key.as_str() == *first) {
        set(inner, rest, new);
    } else {
        assert!(rest.is_empty(), "missing intermediate key {first}");
        members.push(((*first).to_string(), new));
    }
}

fn strings(items: &[&str]) -> Json {
    Json::Array(items.iter().map(|item| Json::string(*item)).collect())
}

fn validate(
    page: &str,
    infra: Option<&Json>,
    ai: Option<&Json>,
    design: Option<&Json>,
) -> Vec<String> {
    validate_report(page, &Artifacts { infra, ai, design }, &Options::default())
}

/// A minimal-fixture run: no table of contents required, no readability.
fn lenient(page: &str) -> Vec<String> {
    let options = Options {
        require_toc: false,
        check_readability: false,
        ..Options::default()
    };
    validate_report(page, &Artifacts::default(), &options)
}

fn exec(values: &[&str]) -> (u8, String, String) {
    let args: Vec<OsString> = values.iter().map(OsString::from).collect();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = run(&args, &mut out, &mut err);
    (
        code,
        String::from_utf8(out).expect("utf-8 stdout"),
        String::from_utf8(err).expect("utf-8 stderr"),
    )
}

// ---------------------------------------------------------------------------
// The tokenizer and html.unescape
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Events(Vec<String>);

impl Handler for Events {
    fn start_tag(&mut self, tag: &str, attrs: &[Attr], text: &str) {
        self.0.push(format!("start {tag} {attrs:?} {text}"));
    }

    fn start_end_tag(&mut self, tag: &str, attrs: &[Attr], text: &str) {
        self.0.push(format!("startend {tag} {attrs:?} {text}"));
    }

    fn end_tag(&mut self, tag: &str) {
        self.0.push(format!("end {tag}"));
    }

    fn data(&mut self, data: &str) {
        self.0.push(format!("data {data:?}"));
    }

    fn comment(&mut self, data: &str) {
        self.0.push(format!("comment {data:?}"));
    }
}

fn events(markup: &str) -> Vec<String> {
    let mut collected = Events::default();
    parse(markup, &mut collected);
    collected.0
}

#[test]
fn tags_attributes_and_data_are_tokenized() {
    assert_eq!(
        events(r#"<P Class="a b" id=x hidden data-k='v'>hi &amp; bye</P>"#),
        vec![
            r#"start p [("class", Some("a b")), ("id", Some("x")), ("hidden", None), ("data-k", Some("v"))] <P Class="a b" id=x hidden data-k='v'>"#,
            r#"data "hi & bye""#,
            "end p",
        ]
    );
    assert_eq!(
        events("<br/><img src=a />x"),
        vec![
            "startend br [] <br/>",
            r#"startend img [("src", Some("a"))] <img src=a />"#,
            r#"data "x""#,
        ]
    );
}

#[test]
fn a_bare_angle_bracket_is_data_and_an_open_tag_is_dropped() {
    assert_eq!(
        events("a < b"),
        vec![r#"data "a ""#, r#"data "<""#, r#"data " b""#]
    );
    assert_eq!(events("text <a href=\"x\""), vec![r#"data "text ""#]);
    assert_eq!(events("</"), vec![r#"data "</""#]);
}

#[test]
fn comments_declarations_and_instructions() {
    assert_eq!(
        events("<!-- a --><!----><!--><!--x--!>"),
        vec![
            r#"comment " a ""#,
            r#"comment """#,
            r#"comment """#,
            r#"comment "x""#,
        ]
    );
    assert_eq!(
        events("<!-- never closed"),
        vec![r#"comment " never closed""#]
    );
    assert_eq!(events("<!-- cut --"), vec![r#"comment " cut ""#]);
    assert_eq!(events("<!DOCTYPE html><?pi x?>a"), vec![r#"data "a""#]);
    assert_eq!(
        events("<![CDATA[ x ]]>y</>z<!bogus>"),
        vec![r#"data "y""#, r#"data "z""#, r#"comment "bogus""#]
    );
}

#[test]
fn raw_text_elements_keep_markup_and_rcdata_decodes() {
    assert_eq!(
        events("<script>if (a < b && c) {}</script>x"),
        vec![
            "start script [] <script>",
            r#"data "if (a < b && c) {}""#,
            "end script",
            r#"data "x""#,
        ]
    );
    assert_eq!(
        events("<style>a>b</STYLE >"),
        vec!["start style [] <style>", r#"data "a>b""#, "end style"]
    );
    assert_eq!(
        events("<title>a &amp; <b>x</title>"),
        vec!["start title [] <title>", r#"data "a & <b>x""#, "end title"]
    );
    // An element that never closes keeps its text to the end.
    assert_eq!(
        events("<script>var a = 1;"),
        vec!["start script [] <script>", r#"data "var a = 1;""#]
    );
}

#[test]
fn quoted_values_may_hold_a_closing_bracket_and_references_decode() {
    assert_eq!(
        events(r#"<a title="x > y" href='&#65;&lt'>t</a>"#),
        vec![
            r#"start a [("title", Some("x > y")), ("href", Some("A<"))] <a title="x > y" href='&#65;&lt'>"#,
            r#"data "t""#,
            "end a",
        ]
    );
    // An unclosed quote lets the whitespace give back a character, so the
    // attribute is empty and the quote starts the next attribute name.
    assert_eq!(
        events("<a b= \"c>d"),
        vec![
            r#"start a [("b", Some("")), ("\"c", None)] <a b= "c>"#,
            r#"data "d""#,
        ]
    );
}

#[test]
fn unescape_follows_the_html5_rules() {
    assert_eq!(unescape("plain"), "plain");
    assert_eq!(unescape("&lt;b&gt; &#65; &#x42; &#X43;"), "<b> A B C");
    assert_eq!(unescape("AT&amp T &amp; &AMP;"), "AT& T & &");
    assert_eq!(unescape("&notit; &notin;"), "¬it; ∉");
    assert_eq!(unescape("&unknown; & &;"), "&unknown; & &;");
    assert_eq!(
        unescape("&#0; &#x80; &#xd800; &#1114112; &#1; &#99999999999999;"),
        "\u{fffd} € \u{fffd} \u{fffd}  \u{fffd}"
    );
    assert_eq!(unescape("&nbsp;&mdash;&rarr;&times;"), "\u{a0}—→×");
    assert_eq!(unescape("&#38;amp;"), "&amp;");
}

// ---------------------------------------------------------------------------
// The extractors
// ---------------------------------------------------------------------------

#[test]
fn a_section_is_found_by_its_parsed_structure() {
    let page = "<section id='a'>x<section>y</section>z</section ><section id=a>again</section>";
    assert_eq!(
        section_html(page, "a").as_deref(),
        Some("x<section>y</section>z")
    );
    assert_eq!(section_html(page, "b"), None);
    let escaped = "<section id=\"s\">1 &lt; 2 <b>bold</b><!-- c --></section>";
    assert_eq!(
        section_html(escaped, "s").as_deref(),
        Some("1 &lt; 2 <b>bold</b><!-- c -->")
    );
    let spaced = "<section\n  id = \"s\">in</section>";
    assert_eq!(section_html(spaced, "s").as_deref(), Some("in"));
}

#[test]
fn plain_text_skips_inert_and_hidden_content() {
    let text = plain_text(
        "<p>A &mdash; B</p><template>x</template><span hidden>y</span> <script>z</script><!-- c -->Q",
    );
    assert_eq!(text, "a - b q");
}

#[test]
fn decoded_text_marks_calculation_cells_and_boundaries() {
    let decoded = decoded_text(
        "<table><thead><tr><th>Item</th><th>Calculation</th></tr></thead><tbody><tr><td>x</td>\
         <td>1 vCPU &times; $23.50</td></tr></tbody></table><script>$9.99</script>",
    );
    assert!(
        decoded.text.contains("1 vCPU × $23.50"),
        "{:?}",
        decoded.text
    );
    assert!(!decoded.text.contains("9.99"));
    let amount = decoded.text.find("$23.50").expect("amount");
    assert!(decoded.calc_mask[amount..amount + 6]
        .iter()
        .all(|flag| *flag));
    let item = decoded.text.find("Item").expect("item");
    assert!(!decoded.calc_mask[item]);
    assert!(!decoded.boundaries.is_empty());
    assert_eq!(decoded.calc_mask.len(), decoded.text.len());
}

#[test]
fn cost_anchors_skip_comments_hidden_and_inert_subtrees() {
    let anchors = cost_anchor_matches(
        "<p><span data-cost-key=\"AWS_Monthly_Balanced\">$1,415</span> \
         <span hidden data-cost-key=\"hid\">$1</span>\
         <template><b data-cost-key=\"tpl\">$2</b></template>\
         <!-- <span data-cost-key=\"cmt\">$3</span> -->\
         <b data-cost-key=\"outer\">a<i data-cost-key=\"inner\">$4</i></b>\
         <u data-cost-key=\"empty\"/></p>",
    );
    assert_eq!(
        anchors,
        vec![
            ("aws_monthly_balanced".to_string(), "$1,415".to_string()),
            ("inner".to_string(), "$4".to_string()),
            ("outer".to_string(), "a$4".to_string()),
            ("empty".to_string(), String::new()),
        ]
    );
}

#[test]
fn the_rendered_fragment_orders_headings_items_and_prose() {
    let rendered = rendered_fragment(
        "<h3>What would flip this</h3><p>Lead</p><ul class=\"compact\"><li>One <em>thing</em></li>\
         <li hidden>No</li><template><li>T</li></template><li>Two</li></ul>",
    );
    assert_eq!(
        rendered.entries,
        vec![
            (Kind::Heading, "what would flip this".to_string()),
            (Kind::Text, "lead".to_string()),
            (Kind::Item, "one thing".to_string()),
            (Kind::Item, "two".to_string()),
        ]
    );
    assert!(rendered.class_tokens.contains("compact"));
    assert!(!rendered.class_tokens.contains("metric"));
}

#[test]
fn the_list_after_a_heading_is_found_in_three_shapes() {
    let entries = |items: &[(Kind, &str)]| -> Vec<(Kind, String)> {
        items
            .iter()
            .map(|(kind, text)| (*kind, (*text).to_string()))
            .collect()
    };
    let heading = entries(&[
        (Kind::Heading, "what would flip this"),
        (Kind::Text, "intro"),
        (Kind::Item, "a"),
        (Kind::Item, "b"),
        (Kind::Text, "after"),
    ]);
    assert_eq!(
        decision::rendered_list_after_heading(&heading, "what would flip"),
        Some(vec!["a".to_string(), "b".to_string()])
    );
    let inline = entries(&[
        (Kind::Item, "what would flip this: first"),
        (Kind::Item, "second"),
    ]);
    assert_eq!(
        decision::rendered_list_after_heading(&inline, "what would flip"),
        Some(vec!["first".to_string(), "second".to_string()])
    );
    let empty = entries(&[(Kind::Heading, "what would flip"), (Kind::Heading, "next")]);
    assert_eq!(
        decision::rendered_list_after_heading(&empty, "what would flip"),
        Some(Vec::new())
    );
    assert_eq!(
        decision::rendered_list_after_heading(&empty, "absent"),
        None
    );
}

// ---------------------------------------------------------------------------
// The structure and style validators
// ---------------------------------------------------------------------------

#[test]
fn section_ids_are_counted_and_required_ones_reported() {
    let page = "<section id=\"a\"></section><SECTION class=x id='b'></SECTION><section id=\"a\"></section>";
    assert_eq!(
        structure::section_id_counts(page),
        vec![("a".to_string(), 2), ("b".to_string(), 1)]
    );
    assert_eq!(
        structure::required_sections(page, &["a", "b", "c"]),
        vec![
            "duplicate <section id=\"a\"> (2 occurrences)".to_string(),
            "missing required <section id=\"c\">".to_string(),
        ]
    );
}

#[test]
fn the_table_of_contents_must_match_the_sections() {
    let page = "<nav class=\"toc x\"><a href=\"#a\">A</a><a href=\"#gone\">G</a></nav>\
                <section id=\"a\"></section><section id=\"b\"></section>";
    assert_eq!(structure::toc_hrefs(page), vec!["a", "gone"]);
    assert_eq!(
        structure::toc_links(page, &["a", "b"]),
        vec![
            "TOC broken link href=\"#gone\" — no matching <section id=\"gone\">".to_string(),
            "TOC missing link to required section id=\"b\" (add <a href=\"#b\">)".to_string(),
        ]
    );
    assert!(structure::toc_links("<section id=\"a\"></section>", &["a"]).is_empty());
    let late = "<nav class=\"toc\"></nav><section id=\"decision-summary\"></section>";
    assert_eq!(structure::decision_first(late).len(), 1);
    assert!(structure::decision_first("<section id=\"decision-summary\"></section>").is_empty());
}

#[test]
fn the_readability_rules_flag_internal_vocabulary() {
    let page = "<body><p>TCO is very high. Rubric: x. 4-6 engineering hours. 3-5 weeks. \
                Legacy planning heuristic (uncalibrated): 3-5 weeks. Section 0 starts on \
                12/31/2026.</p><h2>Section 2 — Plan</h2><p>multi-TCO</p></body>";
    let errors = style::readability(page);
    for needle in [
        "misleading ownership-cost label",
        "internal scoring trace",
        "effort-hours range",
        "bare week-range estimate",
        "literal \"Section 0\" heading",
        "numbered \"Section N —\" heading",
        "vague intensifier",
        "slash-format date",
    ] {
        assert!(
            errors.iter().any(|error| error.contains(needle)),
            "{needle}: {errors:?}"
        );
    }
    assert!(
        style::readability("<body><p>x-TCO and TCO_value and 5-7 weeks</p></body>")
            .iter()
            .any(|error| error.contains("bare week-range"))
    );
    assert!(style::readability("<style>.rubric {}</style><body><p>fine</p></body>").is_empty());
    assert!(style::readability(
        "<body><p>Legacy planning heuristic (uncalibrated): 3–5 weeks</p></body>"
    )
    .is_empty());
}

#[test]
fn cents_on_a_monthly_figure_are_flagged_unless_they_are_rates() {
    let flagged =
        style::currency_formatting("<body><p>Total $25,684.89/mo and $25,684.89</p></body>");
    assert_eq!(flagged.len(), 1);
    assert!(flagged[0].starts_with("currency formatting: \"$25,684.89\" renders cents"));
    for exempt in [
        "<p>$21.18/hr</p>",
        "<p>$0.40 and $1.50</p>",
        "<p>$5.00/mo per policy</p>",
        "<p><strong>$23.50</strong>/hr</p>",
        "<p>$21.18 (1-mo commit)</p>",
        "<!-- $99.99 --><script>$88.88</script>",
    ] {
        assert!(style::currency_formatting(exempt).is_empty(), "{exempt}");
    }
    // A unit that starts the next cell is not this figure's unit.
    assert_eq!(
        style::currency_formatting("<table><tr><td>$21.18</td><td>Hourly note</td></tr></table>")
            .len(),
        1
    );
    let table = |cell: &str| {
        format!(
            "<table><thead><tr><th>Item</th><th>Calculation</th></tr></thead><tbody><tr><td>x</td>\
             <td>{cell}</td></tr></tbody></table>"
        )
    };
    assert!(style::currency_formatting(&table("1 vCPU × $23.50 × 511 hrs")).is_empty());
    assert!(style::currency_formatting(&table("511 hrs × $23.50")).is_empty());
    assert_eq!(
        style::currency_formatting(&table("ALB $22.50 + NAT $33.10")).len(),
        2
    );
    assert!(style::currency_formatting("<p>1 × $23.50</p>").len() == 1);
}

const GOOD_CSS: &str = "body { background: var(--bg); } .report { max-width: 900px; } \
    section { background: #fff; border: 1px solid #ccc; border-radius: 8px; } \
    .metrics { display: grid; grid-template-columns: repeat(3, 1fr); } \
    .metric { border: 1px solid #ccc; border-radius: 8px; } \
    .verdict { background: #eef; border: 1px solid #99c; } .savings { color: green; } \
    .appendix-header { margin: 0; } :focus-visible { outline: 2px solid; } \
    @media (max-width: 700px) { body { margin: 0; } } @media print { body { color: #000; } }";

#[test]
fn the_visual_contract_needs_a_complete_inline_style() {
    assert!(style::visual_contract(&format!("<style>{GOOD_CSS}</style>")).is_empty());
    assert_eq!(
        style::visual_contract("<p>none</p>"),
        vec!["visual readability: missing inline <style> report shell".to_string()]
    );
    let errors = style::visual_contract("<style>body { color: red; }</style>");
    assert_eq!(errors.len(), 11);
    assert_eq!(
        errors[0],
        "visual readability: body must use a contrasting page background"
    );
}

#[test]
fn the_executive_flow_hides_artifact_and_resource_names() {
    let page = "<section id=\"exec-costs\"><p>See Estimation-Infra.json and aws-design.json and \
                aws_lambda_function.api, aws_s3_bucket.logs</p></section>\
                <section id=\"appendix-costs\"><p>estimation-infra.json</p></section>";
    assert_eq!(
        style::exec_vocabulary(page),
        vec![
            "exec vocabulary: <section id=\"exec-costs\"> exposes artifact filename(s) \
             ['aws-design.json', 'estimation-infra.json'] — name what the reader controls in \
             the executive flow; keep artifact filenames in the technical appendices"
                .to_string(),
            "exec vocabulary: <section id=\"exec-costs\"> exposes Terraform resource ID(s) \
             ['aws_lambda_function.api', 'aws_s3_bucket.logs'] — move resource names to the \
             appendix; the executive flow names what the reader controls"
                .to_string(),
        ]
    );
}

#[test]
fn accessibility_checks_language_headings_tables_and_figures() {
    let good = "<html lang=\"en\"><body><h1>T</h1><table><caption>c</caption><tr>\
                <th scope=\"col\">a</th></tr></table><figure role=\"img\" aria-label=\"x\">\
                <figcaption>f</figcaption></figure></body></html>";
    assert!(style::accessibility(good).is_empty());
    let bad = "<html><body><h1>a</h1><h1>b</h1><table><tr><th>a</th><th scope=\"col\">b</th></tr>\
               </table><figure><p>x</p></figure></body></html>";
    assert_eq!(
        style::accessibility(bad),
        vec![
            "accessibility: <html> must declare a valid lang attribute".to_string(),
            "accessibility: report must contain exactly one <h1>".to_string(),
            "accessibility: table 1 must include a <caption>".to_string(),
            "accessibility: table 1 header cells must declare scope=\"col\" or scope=\"row\""
                .to_string(),
            "accessibility: figure 1 must declare role=\"img\"".to_string(),
            "accessibility: figure 1 must have an aria-label".to_string(),
            "accessibility: figure 1 must include a <figcaption>".to_string(),
        ]
    );
}

#[test]
fn fixture_bleed_needs_a_migration_directory() {
    let page = "<body><p>Run 0611-0606 and 0701-0702</p></body>";
    assert!(style::fixture_bleed(page, None).is_empty());
    let errors = style::fixture_bleed(page, Some(Path::new("/runs/0801-0802")));
    assert_eq!(errors.len(), 2);
    assert!(errors[0].starts_with("fixture bleed: reference canary migration ID \"0611-0606\""));
    assert!(errors[1].contains("['0611-0606', '0701-0702'] but --migration-dir is 0801-0802"));
    assert!(style::fixture_bleed(page, Some(Path::new("/runs/0611-0606"))).is_empty());
    assert!(style::fixture_bleed("<body>none</body>", Some(Path::new("/runs/named"))).is_empty());
}

// ---------------------------------------------------------------------------
// Content validators
// ---------------------------------------------------------------------------

#[test]
fn cost_figures_must_match_the_estimate() {
    let estimate = loads(
        r#"{"projected_costs": {"aws_monthly_balanced": 112.9, "aws_monthly_premium": "abc"},
            "current_costs": {"gcp_monthly": 0.4}}"#,
    )
    .expect("json");
    let page =
        "<section id=\"exec-costs\"><span data-cost-key=\"aws_monthly_balanced\">$113</span>\
                <span data-cost-key=\"current_monthly\">$0.40</span></section>";
    assert!(figures::cost_figures(page, Some(&estimate), true).is_empty());
    let wrong =
        "<section id=\"exec-costs\"><span data-cost-key=\"aws_monthly_balanced\">$120</span>\
                 <span data-cost-key=\"aws_monthly_premium\">$5</span>\
                 <span data-cost-key=\"current_monthly\">none</span></section>";
    assert_eq!(
        figures::cost_figures(wrong, Some(&estimate), true),
        vec![
            "cost figure mismatch: data-cost-key=\"aws_monthly_balanced\" renders \"$120\" but \
             estimation-infra.json projected_costs.aws_monthly_balanced = $113"
                .to_string(),
            "estimation-infra.json projected_costs.aws_monthly_premium is not a numeric dollar \
             amount: 'abc'"
                .to_string(),
            "data-cost-key=\"current_monthly\" element renders no dollar amount (expected \
             $0.40 from current_costs.gcp_monthly)"
                .to_string(),
        ]
    );
    // Required figures must be anchored inside exec-costs.
    let missing = "<section id=\"exec-costs\"><p>$113</p></section>";
    let errors = figures::cost_figures(missing, Some(&estimate), true);
    assert_eq!(errors.len(), 2);
    assert!(errors[0].starts_with("missing data-cost-key=\"aws_monthly_balanced\" anchor"));
    assert!(figures::cost_figures(missing, Some(&estimate), false).is_empty());
    assert!(figures::cost_figures(missing, None, true).is_empty());
    assert!(figures::cost_figures(missing, Some(&Json::Array(vec![])), true).is_empty());
}

#[test]
fn the_summary_panels_have_their_own_rules() {
    let summary = |inner: &str| format!("<section id=\"decision-summary\">{inner}</section>");
    let lists = summary(
        "<h3>Key decisions ahead</h3><ul><li>a</li></ul><h3>Next steps</h3><ol><li>b</li></ol>",
    );
    assert_eq!(
        structure::action_lists(&lists),
        vec![
            "decision-summary: \"Key decisions ahead\" must use <ol class=\"compact\"> \
              (ordered action items), not a bullet list"
                .to_string()
        ]
    );
    let activate = summary("<p>Apply for AWS Activate credits</p>");
    assert_eq!(structure::activate_link(&activate).len(), 1);
    let linked =
        summary("<p>AWS Activate <a href='https://aws.amazon.com/startups/credits/'>apply</a></p>");
    assert!(structure::activate_link(&linked).is_empty());
    let badge = summary("<h3>Stay if</h3><p class=\"badge-verdict-go\">x</p>");
    let errors = structure::decision_language(&badge, None);
    assert_eq!(errors.len(), 2);
    assert!(errors[0].contains("\"Stay if\" is ambiguous"));
    let estimate = loads(r#"{"recommendation": {"stay_if": ["a"]}}"#).expect("json");
    let none = structure::decision_language(&summary("<p>x</p>"), Some(&estimate));
    assert_eq!(
        none,
        vec![
            "decision-summary has recommendation.stay_if reasons and must contain exactly one \
              \"Stay entirely if\" heading (found 0)"
                .to_string()
        ]
    );
    assert!(
        structure::decision_language(&summary("<h3>Stay entirely if</h3>"), Some(&estimate))
            .is_empty()
    );
    let verdict = loads(r#"{"recommendation": {"outcome": "go"}}"#).expect("json");
    assert_eq!(
        structure::verdict(&summary("<p>x</p>"), Some(&verdict)).len(),
        1
    );
    assert!(
        structure::verdict(&summary("<p class=\"big verdict\">x</p>"), Some(&verdict)).is_empty()
    );
}

#[test]
fn the_glossary_config_and_share_panels() {
    assert_eq!(structure::glossary_table("<p>x</p>").len(), 1);
    let thin = "<section id=\"appendix-glossary\"><table class=\"glossary-table\"><thead><tr>\
                <th>Term</th><th>Meaning</th></tr></thead><tbody><tr><td>a</td><td>b</td></tr>\
                </tbody></table></section>";
    assert_eq!(
        structure::glossary_table(thin),
        vec!["glossary-table must contain at least 3 term-definition rows".to_string()]
    );
    let rows = "<tr><td>a</td><td>b</td></tr>".repeat(3);
    let full = thin.replace("<tr><td>a</td><td>b</td></tr>", &rows);
    assert!(structure::glossary_table(&full).is_empty());

    let config = "<section id=\"appendix-config\"><table><thead><tr><th>Question</th></tr></thead>\
                  <tbody><tr><td>q</td></tr></tbody></table></section>";
    let errors = structure::appendix_config(config);
    assert_eq!(errors.len(), 4, "{errors:?}");
    assert!(errors[3].contains("found 1"));
    assert!(structure::appendix_config("<p>none</p>").is_empty());

    let words = "word ".repeat(20);
    let share = |inner: &str| format!("<section id=\"exec-share\">{inner}</section>");
    let ok = share(&format!("<div class=\"share-card\"><p>{words}</p></div>"));
    assert!(structure::share_section(&ok, Some(&Json::Null), None).is_empty());
    assert!(structure::share_section(&ok, None, None).is_empty());
    let short = share("<div class=\"share-card\"><p>few words</p><script>x</script></div>");
    let errors = structure::share_section(&short, Some(&Json::Null), None);
    assert!(
        errors[0].contains("at least 20 words (found 2)"),
        "{errors:?}"
    );
    assert!(errors[1].starts_with("exec-share must be a static copy-ready paragraph"));
    assert_eq!(
        structure::share_section("<p>x</p>", Some(&Json::Null), None).len(),
        1
    );
}

#[test]
fn security_content_follows_the_baseline() {
    let estimate = loads(
        r#"{"projected_costs": {"breakdown": {"security_baseline": {"components":
            {"guardduty": 13.0, "cloudtrail": null}}}}}"#,
    )
    .expect("json");
    let scoped = |inner: &str| format!("<section id=\"appendix-security\">{inner}</section>");
    assert!(
        structure::guardduty_or_baseline(&scoped("<p>GuardDuty on</p>"), Some(&estimate)).is_ok()
    );
    assert!(structure::guardduty_or_baseline(&scoped("<p>$13.00</p>"), Some(&estimate)).is_ok());
    assert!(
        structure::guardduty_or_baseline(&scoped("<p>$14</p>"), Some(&estimate))
            .unwrap_err()
            .starts_with("appendix-security/appendix-costs must mention GuardDuty")
    );
    assert_eq!(
        structure::guardduty_or_baseline("<p>none</p>", Some(&estimate)),
        Err("missing security content in appendix-security or appendix-costs sections".to_string())
    );
    assert_eq!(
        structure::security_teaser("<p>x</p>", Some(&estimate)).len(),
        1
    );
    assert!(structure::security_teaser(
        "<section id=\"exec-security-teaser\"></section>",
        Some(&estimate)
    )
    .is_empty());
    assert!(structure::security_teaser("<p>x</p>", None).is_empty());
}

#[test]
fn optimization_opportunities_need_their_sections() {
    let estimate = loads(
        r#"{"optimization_opportunities": [
            {"opportunity": "Compute Savings Plan", "type": "savings_plan"},
            {"opportunity": "RDS", "type": "reserved_instances", "target_services": ["RDS"]}]}"#,
    )
    .expect("json");
    let none = super::optimize::optimization_sections(
        "<p>x</p>",
        Some(&estimate),
        None,
        None,
        Mode::Full,
        true,
    );
    assert_eq!(none.len(), 4, "{none:?}");
    assert!(none[0]
        .starts_with("optimization_opportunities exist but no <section id=\"exec-optimization\">"));
    assert!(none[1].contains("appendix-optimization"));
    assert!(none[2].contains("has no Savings Plans data row"));
    assert!(none[3].contains("has no Reserved Instances data row"));

    let table = |first: &str| {
        format!(
            "<table><thead><tr><th>Optimization</th><th>Target</th><th>Commitment</th>\
             <th>Effort</th><th>Monthly savings</th></tr></thead><tbody><tr><td>{first}</td>\
             <td>t</td></tr></tbody></table>"
        )
    };
    let page = |first: &str| {
        format!(
            "<nav class=\"toc\"><a href=\"#exec-optimization\">a</a>\
             <a href=\"#appendix-optimization\">b</a></nav>\
             <section id=\"exec-optimization\"><p>Balanced baseline; not additive to Optimized.</p></section>\
             <section id=\"appendix-optimization\">{}</section>",
            table(first)
        )
    };
    let rendered = page("Savings Plans and Reserved Instances");
    assert!(super::optimize::optimization_sections(
        &rendered,
        Some(&estimate),
        None,
        None,
        Mode::Full,
        true
    )
    .is_empty());
    let errors = super::optimize::optimization_sections(
        &page("Spot instances"),
        Some(&estimate),
        None,
        None,
        Mode::Full,
        true,
    );
    assert_eq!(errors.len(), 2, "{errors:?}");
    assert!(errors[0].contains("has no Savings Plans data row"));
    assert!(errors[1].contains("has no Reserved Instances data row"));
    assert!(super::optimize::optimization_sections(
        "<p>x</p>",
        Some(&Json::object(vec![])),
        None,
        None,
        Mode::Full,
        true
    )
    .is_empty());
}

#[test]
fn what_if_tables_carry_the_decision_core_columns() {
    let dir = TempDir::new("whatif");
    fs::create_dir_all(dir.join("scenarios")).expect("mkdir");
    fs::write(
        dir.join("scenarios/index.json"),
        r#"{"scenarios": [{"id": "a"}, {"id": "b"}]}"#,
    )
    .expect("write index");
    assert_eq!(decision::scenario_count(Some(&*dir)), 2);
    assert_eq!(decision::scenario_count(None), 0);
    assert_eq!(
        decision::scenario_count(Some(dir.join("absent").as_path())),
        0
    );
}

// ---------------------------------------------------------------------------
// The whole validator
// ---------------------------------------------------------------------------

#[test]
fn minimal_documents_fail_on_missing_sections_only_when_lenient() {
    let errors = lenient("<html><body><p>draft for review</p></body></html>");
    assert!(errors
        .iter()
        .any(|error| error == "missing required <section id=\"decision-summary\">"));
    assert!(!errors.iter().any(|error| error.starts_with("TOC")));
    let marked = lenient("<p>TODO and [placeholder]</p>");
    assert!(marked.contains(&"forbidden content: placeholder text".to_string()));
    assert!(marked.contains(&"forbidden content: TODO marker".to_string()));
    assert!(marked.contains(&"footer must contain \"draft for review\" disclaimer".to_string()));
}

#[test]
fn the_decision_mode_forbids_appendices() {
    let options = Options {
        mode: Mode::Decision,
        ..Options::default()
    };
    let errors = validate_report(
        "<section id=\"appendix-x\"></section>",
        &Artifacts::default(),
        &options,
    );
    assert!(errors.contains(
        &"decision mode forbids <section id=\"appendix-x\"> — the decision report has no \
          appendices; the full migration report (Generate) carries them"
            .to_string()
    ));
    assert!(errors.contains(&"missing required <section id=\"decision-cta\">".to_string()));
}

#[test]
fn the_stub_fixture_must_fail() {
    let stub = fs::read_to_string(fixtures().join("migration-report-stub.html")).expect("stub");
    let errors = validate(&stub, None, None, None);
    for needle in [
        "missing required <section id=\"exec-assumptions\">",
        "readability: literal \"Section 0\" heading",
        "readability: numbered \"Section N —\" heading",
        "readability: internal scoring trace",
        "exec vocabulary: <section id=\"exec-costs\"> exposes artifact filename(s)",
        "visual readability: missing inline <style> report shell",
        "accessibility: report must contain exactly one <h1>",
        "appendix appears to be a stub (links to JSON only)",
        "appendix section id=appendix-steps has insufficient content (0), need >= 2",
    ] {
        assert!(
            errors.iter().any(|error| error.contains(needle)),
            "{needle}: {errors:#?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Decision-core cases (test_validate_migration_report_decision_core.py)
// ---------------------------------------------------------------------------

const FLIP_BLOCK: &str = "      <h3>What would flip this</h3>
      <ul class=\"compact\">
        <li>Single-AZ acceptable: AWS estimate drops further, strengthens go</li>
        <li>BigQuery must cut over in the same window: defer for specialist evidence</li>
      </ul>";

const BIGQUERY_ITEM: &str =
    "        <li>BigQuery must cut over in the same window: defer for specialist evidence</li>\n";

fn flip_errors(page: &str) -> Vec<String> {
    flip_errors_for(page, &reference_estimate())
}

fn flip_errors_for(page: &str, estimate: &Json) -> Vec<String> {
    validate(page, Some(estimate), None, None)
        .into_iter()
        .filter(|error| error.contains("flip"))
        .collect()
}

fn replaced(from: &str, to: &str) -> String {
    let page = reference_html();
    assert!(page.contains(from), "the reference lacks {from:?}");
    page.replace(from, to)
}

#[test]
fn reference_fixture_still_passes() {
    let (estimate, ai) = (reference_estimate(), reference_ai());
    let errors = validate(&reference_html(), Some(&estimate), Some(&ai), None);
    assert!(errors.is_empty(), "{errors:#?}");
}

#[test]
fn missing_would_flip_list_fails() {
    let page = replaced("<h3>What would flip this</h3>", "<h3>Notes</h3>");
    let errors = validate(&page, Some(&reference_estimate()), None, None);
    assert!(
        errors.iter().any(|error| error.contains("would_flip")),
        "{errors:?}"
    );
}

#[test]
fn would_flip_list_only_in_template_fails() {
    let page = replaced(
        "<h3>What would flip this</h3>",
        "<template><h3>What would flip this</h3></template>",
    );
    let errors = validate(&page, Some(&reference_estimate()), None, None);
    assert!(
        errors.iter().any(|error| error.contains("would_flip")),
        "{errors:?}"
    );
}

#[test]
fn would_flip_heading_with_empty_list_fails() {
    let page = replaced(
        FLIP_BLOCK,
        "      <h3>What would flip this</h3>\n      <ul class=\"compact\"></ul>",
    );
    let errors = flip_errors(&page);
    assert!(!errors.is_empty(), "empty flip list passed");
    assert!(errors[0].contains("0 of 2"), "{errors:?}");
}

#[test]
fn would_flip_partial_list_fails() {
    let errors = flip_errors(&replaced(BIGQUERY_ITEM, ""));
    assert!(!errors.is_empty(), "partial flip list passed");
    assert!(
        errors[0].contains("1 of 2") && errors[0].contains("BigQuery"),
        "{errors:?}"
    );
}

#[test]
fn would_flip_shared_suffix_cannot_cross_satisfy_items() {
    let mut estimate = reference_estimate();
    set(
        &mut estimate,
        &["recommendation", "would_flip_if"],
        strings(&[
            "Database changes require specialist evidence",
            "Compute changes require specialist evidence",
        ]),
    );
    let page = replaced(
        FLIP_BLOCK,
        "      <h3>What would flip this</h3>
      <ul class=\"compact\">
        <li>Database changes require specialist evidence</li>
      </ul>",
    );
    let errors = flip_errors_for(&page, &estimate);
    assert!(
        !errors.is_empty() && errors[0].contains("1 of 2") && errors[0].contains("Compute"),
        "{errors:?}"
    );
    let complete = replaced(
        FLIP_BLOCK,
        "      <h3>What would flip this</h3>
      <ul class=\"compact\">
        <li>Database changes require specialist evidence</li>
        <li>Compute changes require specialist evidence</li>
      </ul>",
    );
    assert!(flip_errors_for(&complete, &estimate).is_empty());
}

#[test]
fn would_flip_items_only_in_template_fail() {
    let page = replaced(
        "      <ul class=\"compact\">\n        <li>Single-AZ acceptable",
        "      <ul class=\"compact\"><template><li>hidden</li></template>\n        <li>Single-AZ acceptable",
    )
    .replace(
        BIGQUERY_ITEM,
        "        <template><li>BigQuery must cut over in the same window: defer for specialist evidence</li></template>\n",
    );
    let errors = flip_errors(&page);
    assert!(
        !errors.is_empty() && errors[0].contains("BigQuery"),
        "{errors:?}"
    );
}

#[test]
fn would_flip_complete_list_passes_with_inline_markup() {
    let page = replaced(
        FLIP_BLOCK,
        "      <h3>What&nbsp;<em>would</em>\n flip this</h3>
      <ul class=\"compact\">
        <li><strong>Single-AZ</strong> acceptable: AWS estimate drops further, strengthens go</li>
        <li>BigQuery must cut over in the same window: defer for specialist evidence</li>
      </ul>",
    );
    assert!(flip_errors(&page).is_empty());
}

const LEAD_IN: &str = "      <p class=\"muted\">Any of these would change the verdict:</p>\n";

#[test]
fn would_flip_lead_in_paragraph_before_list_passes() {
    let page = replaced(
        "      <h3>What would flip this</h3>\n",
        &format!("      <h3>What would flip this</h3>\n{LEAD_IN}"),
    );
    assert!(flip_errors(&page).is_empty());
}

#[test]
fn would_flip_lead_in_paragraph_with_empty_list_fails() {
    let page = replaced(
        FLIP_BLOCK,
        &format!("      <h3>What would flip this</h3>\n{LEAD_IN}      <ul class=\"compact\"></ul>"),
    );
    let errors = flip_errors(&page);
    assert!(
        !errors.is_empty() && errors[0].contains("0 of 2"),
        "{errors:?}"
    );
}

#[test]
fn would_flip_another_heading_before_any_item_is_empty_list() {
    let page = replaced(
        FLIP_BLOCK,
        "      <h3>What would flip this</h3>\n      <p>See below.</p>",
    );
    let errors = flip_errors(&page);
    assert!(
        !errors.is_empty() && errors[0].contains("0 of 2"),
        "{errors:?}"
    );
}

const INLINE_LABEL_BLOCK: &str = "      <ul class=\"compact\">
        <li>What would flip this: Single-AZ acceptable: AWS estimate drops further, strengthens go</li>
        <li>BigQuery must cut over in the same window: defer for specialist evidence</li>
      </ul>";

#[test]
fn would_flip_inline_label_item_passes() {
    assert!(flip_errors(&replaced(FLIP_BLOCK, INLINE_LABEL_BLOCK)).is_empty());
}

#[test]
fn would_flip_inline_label_partial_list_fails() {
    let page = replaced(FLIP_BLOCK, &INLINE_LABEL_BLOCK.replace(BIGQUERY_ITEM, ""));
    let errors = flip_errors(&page);
    assert!(
        !errors.is_empty() && errors[0].contains("1 of 2") && errors[0].contains("BigQuery"),
        "{errors:?}"
    );
}

fn with_condition(condition: &str, rendered: bool) -> (String, Json) {
    let mut estimate = reference_estimate();
    set(
        &mut estimate,
        &["recommendation", "conditions"],
        strings(&[condition]),
    );
    let page = if rendered {
        replaced(
            "<li>Condition: confirm CUD expiration date before committing a migration start date</li>",
            &format!("<li>Condition: {condition}</li>"),
        )
    } else {
        reference_html()
    };
    (page, estimate)
}

fn condition_errors(page: &str, estimate: &Json) -> Vec<String> {
    validate(page, Some(estimate), None, None)
        .into_iter()
        .filter(|error| error.contains("condition"))
        .collect()
}

#[test]
fn short_rendered_conditions_pass() {
    for condition in ["Confirm capacity", "Complete load testing", "Confirm"] {
        let (page, estimate) = with_condition(condition, true);
        let errors = condition_errors(&page, &estimate);
        assert!(errors.is_empty(), "{condition}: {errors:?}");
    }
}

#[test]
fn omitted_conditions_fail() {
    for condition in [
        "Confirm capacity",
        "Complete a load test against the Balanced sizing before cutover",
    ] {
        let (page, estimate) = with_condition(condition, false);
        assert!(
            !condition_errors(&page, &estimate).is_empty(),
            "{condition}"
        );
    }
}

fn without_confirm_in_checklist(page: &str) -> String {
    page.replace("<li>Condition: confirm", "<li>Condition: verify")
        .replace("not confirmed", "not verified")
        .replace("<li>Confirm Bedrock", "<li>Verify Bedrock")
        .replace(
            "<p class=\"verdict-headline\">Go, with conditions</p>",
            "<p class=\"verdict-headline\">Go, with conditions</p>\n      <p>Confirm these before cutover.</p>",
        )
}

#[test]
fn one_word_condition_only_in_prose_fails_and_in_checklist_passes() {
    let (page, estimate) = with_condition("Confirm", false);
    let page = without_confirm_in_checklist(&page);
    let summary = section_html(&page, "decision-summary").expect("summary");
    let rendered = rendered_fragment(&summary);
    assert!(rendered
        .entries
        .iter()
        .any(|(kind, text)| *kind != Kind::Item && text.contains("confirm")));
    assert!(!rendered
        .entries
        .iter()
        .any(|(kind, text)| *kind == Kind::Item && text.contains("confirm")));
    assert!(!condition_errors(&page, &estimate).is_empty());
    let fixed = page.replace("<li>Condition: verify CUD", "<li>Condition: Confirm CUD");
    let errors = condition_errors(&fixed, &estimate);
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn verdict_headline_and_metric_hero_must_be_rendered() {
    let estimate = reference_estimate();
    let headline = "<p class=\"verdict-headline\">Go, with conditions</p>";
    for replacement in [
        "<template><p class=\"verdict-headline\">Go, with conditions</p></template>",
        "<p hidden class=\"verdict-headline\">Go, with conditions</p>",
        "<div hidden><p class=\"verdict-headline\">Go, with conditions</p></div>",
    ] {
        let errors = validate(
            &replaced(headline, replacement),
            Some(&estimate),
            None,
            None,
        );
        assert!(
            errors
                .iter()
                .any(|error| error.contains("verdict-headline")),
            "{replacement}"
        );
    }
    for hiding in [
        "<template><div class=\"metric metric-hero\"></div></template>",
        "<div hidden><div class=\"metric metric-hero\"></div></div>",
    ] {
        let page = replaced(
            "<div class=\"metric metric-hero\">",
            "<div class=\"metric\">",
        )
        .replace(
            "<div class=\"metrics\">",
            &format!("{hiding}\n      <div class=\"metrics\">"),
        );
        let errors = validate(&page, Some(&estimate), None, None);
        assert!(
            errors.iter().any(|error| error.contains("metric-hero")),
            "{hiding}"
        );
    }
}

#[test]
fn metric_hero_unquoted_class_passes() {
    let page = replaced(
        "<div class=\"metric metric-hero\">",
        "<div class='metric metric-hero'>",
    );
    let errors = validate(&page, Some(&reference_estimate()), None, None);
    assert!(
        !errors.iter().any(|error| error.contains("metric-hero")),
        "{errors:?}"
    );
}

#[test]
fn would_flip_populated_list_only_in_hidden_element_or_ancestor_fails() {
    for replacement in [
        "      <h3>What would flip this</h3>\n      <ul hidden>\n        <li>Single-AZ acceptable: AWS estimate drops further, strengthens go</li>\n        <li>BigQuery must cut over in the same window: defer for specialist evidence</li>\n      </ul>".to_string(),
        format!("<div hidden>{FLIP_BLOCK}</div>"),
    ] {
        assert!(!flip_errors(&replaced(FLIP_BLOCK, &replacement)).is_empty(), "{replacement}");
    }
}

#[test]
fn deferred_service_without_callout_fails() {
    let page = reference_html()
        .replace("specialist engagement", "analytics follow-up")
        .replace("Specialist engagement", "Analytics follow-up");
    let design = loads(
        r#"{"clusters": [{"resources": [{"aws_service": "Deferred — specialist engagement"}]}]}"#,
    )
    .expect("json");
    let errors = validate(&page, Some(&reference_estimate()), None, Some(&design));
    assert!(
        errors
            .iter()
            .any(|error| error.to_lowercase().contains("specialist")),
        "{errors:?}"
    );
}

#[test]
fn clusters_without_architecture_section_fail() {
    let page = replaced("id=\"exec-architecture\"", "id=\"not-architecture\"");
    let design =
        loads(r#"{"clusters": [{"resources": [{"aws_service": "Amazon S3"}]}]}"#).expect("json");
    let errors = validate(&page, Some(&reference_estimate()), None, Some(&design));
    assert!(
        errors
            .iter()
            .any(|error| error.contains("exec-architecture")),
        "{errors:?}"
    );
}

#[test]
fn scenario_column_check_requires_migration_dir() {
    let page = replaced(
        "<section id=\"exec-timeline\">",
        "<section id=\"what-if-scenarios\"><table><caption>Scenarios</caption><thead><tr>\
         <th scope=\"col\">Scenario</th><th scope=\"col\">Monthly</th>\
         </tr></thead><tbody><tr><td>Baseline</td><td>$1</td></tr></tbody></table></section>\
         <section id=\"exec-timeline\">",
    );
    let (estimate, ai) = (reference_estimate(), reference_ai());
    let without_dir = validate(&page, Some(&estimate), Some(&ai), None);
    assert!(without_dir.is_empty(), "{without_dir:?}");

    let dir = TempDir::new("scenarios");
    fs::create_dir_all(dir.join("scenarios")).expect("mkdir");
    fs::write(
        dir.join("scenarios/index.json"),
        r#"{"scenarios": [{"id": "a"}, {"id": "b"}]}"#,
    )
    .expect("write index");
    let options = Options {
        migration_dir: Some(&*dir),
        ..Options::default()
    };
    let artifacts = Artifacts {
        infra: Some(&estimate),
        ai: Some(&ai),
        design: None,
    };
    let errors = validate_report(&page, &artifacts, &options);
    assert!(
        errors.iter().any(|error| error.contains("Region")),
        "{errors:?}"
    );
}

#[test]
fn decision_fixture_still_passes() {
    let root = fixtures().join("gcp-decision-gate/after-decide-complete");
    let page = fs::read_to_string(root.join("decision-report.html")).expect("decision report");
    let estimate = json_file(&root.join("estimation-infra.json"));
    let options = Options {
        migration_dir: Some(root.as_path()),
        mode: Mode::Decision,
        ..Options::default()
    };
    let artifacts = Artifacts {
        infra: Some(&estimate),
        ai: None,
        design: None,
    };
    let errors = validate_report(&page, &artifacts, &options);
    assert!(errors.is_empty(), "{errors:#?}");
}

// ---------------------------------------------------------------------------
// The command
// ---------------------------------------------------------------------------

fn fixture_arg(name: &str) -> String {
    fixtures().join(name).to_string_lossy().into_owned()
}

#[test]
fn the_command_prints_report_ok_for_the_reference() {
    let report = fixture_arg("migration-report-reference.html");
    let infra = fixture_arg("estimation-infra-reference.json");
    let ai = fixture_arg("estimation-ai-reference.json");
    let (code, out, err) = exec(&[
        "startup",
        "migration-report",
        &report,
        "--estimation-infra",
        &infra,
        "--estimation-ai",
        &ai,
    ]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(err, "");
    assert!(
        out.starts_with("REPORT_OK | structure=complete | sections=10/10"),
        "{out}"
    );
    assert!(
        out.ends_with(" | note=verify dollar figures against estimation JSON before sign-off\n"),
        "{out}"
    );
}

#[test]
fn the_command_reports_failures_on_stderr() {
    let stub = fixture_arg("migration-report-stub.html");
    let (code, out, err) = exec(&["startup", "migration-report", &stub]);
    assert_eq!(code, 1);
    assert_eq!(out, "");
    assert!(
        err.starts_with("REPORT_FAIL | migration-report.html\n  - "),
        "{err}"
    );
    assert!(err.contains("\n  - missing required <section id=\"exec-assumptions\">\n"));
    assert!(err.contains("\nerror: "), "{err}");

    let dir = TempDir::new("missing");
    let absent = dir.join("absent.html");
    let absent = absent.to_string_lossy().into_owned();
    let (code, out, err) = exec(&["startup", "migration-report", &absent]);
    assert_eq!(code, 1);
    assert_eq!(out, "");
    assert!(
        err.starts_with(&format!("REPORT_FAIL | file={absent} | reason=not_found\n")),
        "{err}"
    );
}

#[test]
fn the_command_checks_its_arguments() {
    let (code, _, err) = exec(&["startup", "migration-report"]);
    assert_eq!(code, 2);
    assert!(
        err.contains("the following arguments are required: report_path"),
        "{err}"
    );
    let stub = fixture_arg("migration-report-stub.html");
    let (code, _, err) = exec(&["startup", "migration-report", &stub, "--mode", "other"]);
    assert_eq!(code, 2);
    assert!(err.contains("invalid choice: 'other'"), "{err}");

    // A malformed estimate is an input error, not a pass.
    let dir = TempDir::new("badjson");
    let bad = dir.join("bad.json");
    fs::write(&bad, "{not json").expect("write");
    let bad = bad.to_string_lossy().into_owned();
    let (code, out, err) = exec(&[
        "startup",
        "migration-report",
        &stub,
        "--estimation-infra",
        &bad,
    ]);
    assert_eq!(code, 1);
    assert_eq!(out, "");
    assert!(err.contains("bad.json"), "{err}");
}

#[test]
fn the_decision_mode_line_names_its_mode() {
    let root = fixtures().join("gcp-decision-gate/after-decide-complete");
    let report = root
        .join("decision-report.html")
        .to_string_lossy()
        .into_owned();
    let infra = root
        .join("estimation-infra.json")
        .to_string_lossy()
        .into_owned();
    let dir = root.to_string_lossy().into_owned();
    let (code, out, err) = exec(&[
        "startup",
        "migration-report",
        &report,
        "--estimation-infra",
        &infra,
        "--migration-dir",
        &dir,
        "--mode",
        "decision",
    ]);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.starts_with("REPORT_OK | mode=decision | structure=complete | sections=7/7"),
        "{out}"
    );
}

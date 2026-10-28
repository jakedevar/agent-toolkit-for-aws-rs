//! Declarative substring contracts with derived mutations.
//!
//! Most startup contracts say "this region of this file contains X and not Y".
//! A [`Rule`] states that; [`check_rules`] runs the rules on the committed
//! plugin and then derives one broken copy per assertion (the required text
//! removed from its region, or the forbidden text inserted into it) and
//! requires the rules to fail on it, so every assertion is non-vacuous.

use crate::common::{ensure, name, Plugin, Verdict};

/// Which part of a file a rule inspects, with Python slicing semantics.
#[derive(Clone, Copy)]
pub enum Scope {
    /// The whole file.
    Whole,
    /// `text[text.index(a):text.index(b)]`.
    Between(&'static str, &'static str),
    /// `text[i:i + n]` with `i = text.index(a)` (n counts characters).
    Window(&'static str, usize),
    /// `text[text.index(a):]`.
    From(&'static str),
    /// `i = text.index(a); j = text.index(b, i); text[j:j + n]`.
    AfterWindow(&'static str, &'static str, usize),
    /// `i = text.index(a); j = text.index(b, i); text[i:j]`.
    Span(&'static str, &'static str),
    /// `r = text.index(root); a2 = text.index(a, r); b2 = text.index(b, r); text[a2:b2]` (empty when reversed).
    Within(&'static str, &'static str, &'static str),
    /// `r = text.index(root); i = text.index(a, r); text[i:i + n]`.
    WindowIn(&'static str, &'static str, usize),
    /// `m = text.index(root); i = text.index(b, text.index(a, m)); text[i:text.index("\n", i)]`.
    LineAfter(&'static str, &'static str, &'static str),
    /// `r = text.index(root); s = text.index(a, r); text[s:]`.
    FromIn(&'static str, &'static str),
    /// The YAML front matter: `re.match(r"^---\n(.*?)\n---\n", text, re.DOTALL)` group 1.
    Front,
}

/// One region of one file and what it must and must not contain.
#[derive(Clone)]
pub struct Rule {
    pub file: &'static str,
    pub scope: Scope,
    /// Collapse whitespace runs to one space before matching (`re.sub(r"\s+", " ")`).
    pub norm: bool,
    /// Match case-insensitively (`text.lower()`); needles must then be lower case.
    pub ci: bool,
    /// Mutations of this rule only have to make the rules fail, for any reason (its
    /// needle is a substring of another rule's, so the other rule may trip first).
    pub loose: bool,
    pub all: Vec<&'static str>,
    /// Groups of alternatives: at least one of each group must be present.
    pub any: Vec<Vec<&'static str>>,
    pub none: Vec<&'static str>,
    /// Pairs `(a, b)`: both present and the first `a` precedes the first `b`.
    pub before: Vec<(&'static str, &'static str)>,
}

impl Rule {
    pub fn new(file: &'static str) -> Self {
        Self {
            file,
            scope: Scope::Whole,
            norm: false,
            ci: false,
            loose: false,
            all: Vec::new(),
            any: Vec::new(),
            none: Vec::new(),
            before: Vec::new(),
        }
    }
    pub fn scope(mut self, scope: Scope) -> Self {
        self.scope = scope;
        self
    }
    pub fn norm(mut self) -> Self {
        self.norm = true;
        self
    }
    pub fn loose(mut self) -> Self {
        self.loose = true;
        self
    }
    pub fn ci(mut self) -> Self {
        self.ci = true;
        self
    }
    pub fn all(mut self, needles: &[&'static str]) -> Self {
        self.all.extend_from_slice(needles);
        self
    }
    pub fn any(mut self, alternatives: &[&'static str]) -> Self {
        self.any.push(alternatives.to_vec());
        self
    }
    pub fn before(mut self, first: &'static str, second: &'static str) -> Self {
        self.before.push((first, second));
        self
    }
    pub fn none(mut self, needles: &[&'static str]) -> Self {
        self.none.extend_from_slice(needles);
        self
    }
}

fn find_from(text: &str, needle: &str, from: usize) -> Result<usize, String> {
    text.get(from..)
        .and_then(|t| t.find(needle))
        .map(|i| i + from)
        .ok_or_else(|| format!("{needle:?} not found"))
}

fn char_end(text: &str, start: usize, n: usize) -> usize {
    text[start..]
        .char_indices()
        .nth(n)
        .map_or(text.len(), |(i, _)| start + i)
}

/// Byte range of the region `scope` selects in `text`.
fn region(text: &str, scope: Scope) -> Result<(usize, usize), String> {
    Ok(match scope {
        Scope::Whole => (0, text.len()),
        Scope::Between(a, b) => {
            let s = find_from(text, a, 0)?;
            let e = find_from(text, b, 0)?;
            (s, e.max(s))
        }
        Scope::Window(a, n) => {
            let s = find_from(text, a, 0)?;
            (s, char_end(text, s, n))
        }
        Scope::From(a) => (find_from(text, a, 0)?, text.len()),
        Scope::AfterWindow(a, b, n) => {
            let i = find_from(text, a, 0)?;
            let j = find_from(text, b, i)?;
            (j, char_end(text, j, n))
        }
        Scope::Span(a, b) => {
            let i = find_from(text, a, 0)?;
            (i, find_from(text, b, i)?)
        }
        Scope::Within(root, a, b) => {
            let r = find_from(text, root, 0)?;
            let s = find_from(text, a, r)?;
            let e = find_from(text, b, r)?;
            (s, e.max(s))
        }
        Scope::WindowIn(root, a, n) => {
            let r = find_from(text, root, 0)?;
            let s = find_from(text, a, r)?;
            (s, char_end(text, s, n))
        }
        Scope::LineAfter(root, a, b) => {
            let r = find_from(text, root, 0)?;
            let m = find_from(text, a, r)?;
            let i = find_from(text, b, m)?;
            (i, find_from(text, "\n", i)?)
        }
        Scope::FromIn(root, a) => {
            let r = find_from(text, root, 0)?;
            (find_from(text, a, r)?, text.len())
        }
        Scope::Front => front_matter(text)?,
    })
}

/// Byte range of the front-matter body (between the first two `---` fences).
fn front_matter(text: &str) -> Result<(usize, usize), String> {
    if !text.starts_with("---\n") {
        return Err("file has no YAML frontmatter".into());
    }
    let end =
        find_from(text, "\n---\n", 4).map_err(|_| "file has no YAML frontmatter".to_string())?;
    Ok((4, end))
}

fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn eval_rule(p: &Plugin, rule: &Rule) -> Verdict {
    let raw = p.read(rule.file)?;
    let text = if rule.norm { collapse(&raw) } else { raw };
    let (s, e) = region(&text, rule.scope).map_err(|m| format!("{}: {m}", name(rule.file)))?;
    let scope = &text[s..e];
    let lowered = scope.to_lowercase();
    let scope = if rule.ci { lowered.as_str() } else { scope };
    for needle in &rule.all {
        ensure!(
            scope.contains(needle),
            "{}: missing {needle:?}",
            name(rule.file)
        );
    }
    for group in &rule.any {
        ensure!(
            group.iter().any(|n| scope.contains(n)),
            "{}: none of {group:?} present",
            name(rule.file)
        );
    }
    for needle in &rule.none {
        ensure!(
            !scope.contains(needle),
            "{}: forbidden {needle:?} present",
            name(rule.file)
        );
    }
    for (first, second) in &rule.before {
        let a = scope
            .find(first)
            .ok_or_else(|| format!("{}: missing {first:?}", name(rule.file)))?;
        let b = scope
            .find(second)
            .ok_or_else(|| format!("{}: missing {second:?}", name(rule.file)))?;
        ensure!(
            a < b,
            "{}: {first:?} not before {second:?}",
            name(rule.file)
        );
    }
    Ok(())
}

/// Evaluate every rule against the plugin view.
pub fn eval(p: &Plugin, rules: &[Rule]) -> Verdict {
    rules.iter().try_for_each(|r| eval_rule(p, r))
}

/// `raw` with `needle` replaced by `to` inside the rule's region.
fn rewrite(raw: &str, rule: &Rule, needle: &str, to: &str) -> String {
    let words = needle
        .split(' ')
        .map(fancy_regex::escape)
        .collect::<Vec<_>>();
    let body = if rule.norm {
        words.join(r"\s+")
    } else {
        words.join(" ")
    };
    let pattern = if rule.ci { format!("(?i){body}") } else { body };
    let re = fancy_regex::Regex::new(&pattern).expect("pattern");
    if rule.norm {
        return re.replace_all(raw, to).into_owned();
    }
    let (s, e) = region(raw, rule.scope).expect("region of the committed file");
    format!(
        "{}{}{}",
        &raw[..s],
        re.replace_all(&raw[s..e], to),
        &raw[e..]
    )
}

/// Length in bytes of the anchor text a region starts with.
fn anchor_len(scope: Scope) -> usize {
    match scope {
        Scope::Whole => 0,
        Scope::Front => 4,
        Scope::Between(a, _)
        | Scope::Window(a, _)
        | Scope::From(a)
        | Scope::Span(a, _)
        | Scope::Within(_, a, _)
        | Scope::WindowIn(_, a, _)
        | Scope::FromIn(_, a) => a.len(),
        Scope::AfterWindow(_, b, _) | Scope::LineAfter(_, _, b) => b.len(),
    }
}

/// `raw` with `needle` inserted right after the anchor that opens the rule's
/// region, so the insertion lies inside the region.
fn inject(raw: &str, rule: &Rule, needle: &str) -> String {
    if rule.norm {
        return format!("{needle} {raw}");
    }
    let (s, _) = region(raw, rule.scope).expect("region of the committed file");
    let at = s + anchor_len(rule.scope);
    format!("{} {needle} {}", &raw[..at], &raw[at..])
}

/// Run `rules` on the committed plugin, then on one broken copy per
/// assertion; each broken copy must fail for the reason that assertion names.
///
/// # Panics
/// When a rule rejects the committed plugin or accepts a broken copy.
pub fn check_rules(rules: &[Rule]) {
    let real = Plugin::real();
    if let Err(reason) = eval(&real, rules) {
        panic!("the committed plugin breaks the contract: {reason}");
    }
    for rule in rules {
        let raw = real.read(rule.file).expect("rule file");
        let mut broken: Vec<(String, String)> = Vec::new();
        for needle in &rule.all {
            broken.push((
                rewrite(&raw, rule, needle, "\u{2022}"),
                format!("missing {needle:?}"),
            ));
        }
        for group in &rule.any {
            let mut text = raw.clone();
            for alt in group {
                text = rewrite(&text, rule, alt, "\u{2022}");
            }
            broken.push((text, format!("none of {group:?}")));
        }
        for needle in &rule.none {
            broken.push((inject(&raw, rule, needle), format!("forbidden {needle:?}")));
        }
        for (first, second) in &rule.before {
            broken.push((
                rewrite(&raw, rule, first, "\u{2022}"),
                format!("missing {first:?}"),
            ));
            broken.push((
                inject(&raw, rule, second),
                format!("{first:?} not before {second:?}"),
            ));
        }
        for (text, expected) in broken {
            assert_ne!(
                text, raw,
                "a mutation of {} changed nothing ({expected})",
                rule.file
            );
            let view = real.with_edit(rule.file, |_| text);
            match eval(&view, rules) {
                Ok(()) => panic!(
                    "a copy of {} broken as [{expected}] still passes",
                    rule.file
                ),
                Err(reason) => assert!(
                    rule.loose || reason.contains(&expected),
                    "copy of {} broken as [{expected}] failed for another reason: {reason}",
                    rule.file
                ),
            }
        }
    }
}

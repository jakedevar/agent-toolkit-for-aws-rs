//! Contract documents and the manifest: fenced JSON/JSONC example blocks
//! become key-path templates; the manifest maps artifact names to them.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use toolkit_core::Json;

use super::jsonc::{remove_trailing_commas, replace_lone_ellipsis, strip_jsonc};
use super::paths::Origin;
use super::patterns::{fnmatch, heading_matches, heading_token};
use super::pyjson::{loads, JsonError};
use super::schema::{unsupported_keywords, SubsetValidator};
use super::shape::{
    build_node, mark_allowed_anywhere, merge_example_at, walk_path, KeyComments, Node,
};
use super::{Code, Finding};
use crate::manifests::{is_falsy, is_py_space, py_eq, py_splitlines, py_strip, resolve_lenient};
use crate::startup::text::is_word;

/// The plugin root and the caches one run shares.
pub(crate) struct Env {
    pub(crate) root: PathBuf,
    pub(crate) validator: SubsetValidator,
}

impl Env {
    pub(crate) fn new(root: &Path) -> Self {
        Self {
            root: resolve_lenient(root),
            validator: SubsetValidator::default(),
        }
    }

    /// `_rel`: relative to the plugin root, absolute outside it.
    pub(crate) fn rel(&self, path: &Path) -> String {
        let resolved = resolve_lenient(path);
        match resolved.strip_prefix(&self.root) {
            Ok(inside) if inside.as_os_str().is_empty() => ".".to_string(),
            Ok(inside) => inside.display().to_string(),
            Err(_) => resolved.display().to_string(),
        }
    }
}

/// One fenced JSON/JSONC block.
pub(crate) struct Block {
    pub(crate) heading: String,
    pub(crate) text: String,
    /// 1-based line of the opening fence.
    pub(crate) line: usize,
}

/// `_FENCE_RE`: ```` ^```(\w+)?\s*$ ````; returns the language.
fn fence_lang(line: &str) -> Option<String> {
    let rest = line.strip_prefix("```")?;
    let word: String = rest.chars().take_while(|c| is_word(*c)).collect();
    rest[word.len()..]
        .chars()
        .all(is_py_space)
        .then(|| word.to_lowercase())
}

/// `_scan_doc`: the JSON blocks and every heading in document order.
pub(crate) fn scan_doc(text: &str) -> (Vec<Block>, Vec<String>) {
    let mut blocks = Vec::new();
    let mut headings = Vec::new();
    let mut heading = String::new();
    let mut open: Option<(String, usize)> = None;
    let mut buffer: Vec<&str> = Vec::new();
    for (index, line) in py_splitlines(text).into_iter().enumerate() {
        let number = index + 1;
        match &open {
            None => {
                if line.starts_with('#') {
                    heading = py_strip(line.trim_start_matches('#')).to_string();
                    headings.push(heading.clone());
                }
                if let Some(lang) = fence_lang(line) {
                    open = Some((lang, number));
                    buffer.clear();
                }
            }
            Some((lang, start)) => {
                if line.starts_with("```") {
                    if lang == "json" || lang == "jsonc" {
                        blocks.push(Block {
                            heading: heading.clone(),
                            text: buffer.join("\n"),
                            line: *start,
                        });
                    }
                    open = None;
                } else {
                    buffer.push(line);
                }
            }
        }
    }
    (blocks, headings)
}

fn comment_free(line: &str) -> &str {
    line.split_once("//").map_or(line, |(before, _)| before)
}

/// `parse_block`: the example value and the comments by key they annotate.
pub(crate) fn parse_block(
    block: &Block,
    doc_rel: &str,
    findings: &mut Vec<Finding>,
) -> Option<(Json, KeyComments)> {
    let stripped = strip_jsonc(&block.text);
    let json_text = &stripped.json;
    let lone = replace_lone_ellipsis(json_text);
    let candidates = [
        lone.clone(),
        json_text.clone(),
        format!("{{{json_text}}}"),
        format!("{{{lone}}}"),
    ];
    let mut value: Option<Json> = None;
    let mut first_error: Option<JsonError> = None;
    for candidate in &candidates {
        match loads(&remove_trailing_commas(candidate)) {
            Ok(parsed) => {
                value = Some(parsed);
                break;
            }
            Err(error) => {
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
    }
    // A JSON `null` block counts as "no value", like Python's `value is None`.
    let value = match value {
        Some(Json::Null) | None => {
            if let Ok(parsed) = loads(&format!("{{{json_text}}}")) {
                parsed
            } else {
                let detail = first_error.map_or_else(
                    || "the block is JSON null".to_string(),
                    |e| format!("{} (block line {})", e.msg, e.lineno),
                );
                findings.push(Finding::new(
                    Code::SchemaParse,
                    doc_rel,
                    &format!("L{}", block.line),
                    format!(
                        "example block under '{}' is not valid JSON/JSONC: {detail}",
                        block.heading
                    ),
                    doc_rel,
                ));
                return None;
            }
        }
        Some(parsed) => parsed,
    };
    let raw_lines = py_splitlines(&block.text);
    let mut key_comments = KeyComments::new();
    for (line, comment) in &stripped.comments {
        let mut key = stripped.keys.get(line).cloned();
        if key.is_none() {
            // A comment on its own line inside an object belongs to the key
            // that opened it.
            let mut previous = line.checked_sub(1);
            while let Some(at) = previous {
                if !py_strip(comment_free(raw_lines[at])).is_empty() {
                    break;
                }
                previous = at.checked_sub(1);
            }
            if let Some(at) = previous {
                if comment_free(raw_lines[at])
                    .trim_end_matches(is_py_space)
                    .ends_with('{')
                {
                    key = stripped.keys.get(&at).cloned();
                }
            }
        }
        if let Some(key) = key {
            key_comments.entry(key).or_default().push(comment.clone());
        }
    }
    Some((value, key_comments))
}

/// A parsed shape contract.
pub(crate) struct ShapeContract {
    pub(crate) root: Node,
    pub(crate) label: String,
}

/// A contract for one artifact glob.
pub(crate) struct Contract {
    pub(crate) glob: String,
    pub(crate) shape: Option<ShapeContract>,
    pub(crate) json_schema: Option<Json>,
    pub(crate) json_schema_path: String,
    /// `(json path, expected value, shape)`; the first match replaces the default.
    pub(crate) routes: Vec<(String, Json, ShapeContract)>,
}

fn required_str<'a>(spec: &'a Json, key: &str) -> Result<&'a str, String> {
    spec.get(key)
        .and_then(Json::as_str)
        .ok_or_else(|| format!("manifest entry is missing the string field '{key}'"))
}

fn strings<'a>(spec: &'a Json, key: &str) -> Result<Vec<&'a str>, String> {
    match spec.get(key) {
        None | Some(Json::Null) => Ok(Vec::new()),
        Some(Json::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .ok_or_else(|| format!("manifest field '{key}' must hold strings"))
            })
            .collect(),
        Some(_) => Err(format!("manifest field '{key}' must be a list")),
    }
}

/// Reads a contract document, or `None` after a `not found` finding.
fn read_doc(
    env: &Env,
    doc: &Path,
    missing: &str,
    findings: &mut Vec<Finding>,
) -> Result<Option<String>, String> {
    let shown = env.rel(doc);
    let origin = Origin::Discovered(&env.root);
    if !origin.is_file(doc)? {
        findings.push(Finding::new(
            Code::SchemaParse,
            &shown,
            "",
            missing.to_string(),
            &shown,
        ));
        return Ok(None);
    }
    origin.read(doc, &shown).map(Some)
}

/// `build_shape_contract`.
#[allow(clippy::too_many_lines)]
pub(crate) fn build_shape_contract(
    env: &mut Env,
    spec: &Json,
    findings: &mut Vec<Finding>,
) -> Result<Option<ShapeContract>, String> {
    let doc = env.root.join(required_str(spec, "shape")?);
    let doc_rel = env.rel(&doc);
    let Some(text) = read_doc(env, &doc, "contract document not found", findings)? else {
        return Ok(None);
    };
    let (blocks, headings) = scan_doc(&text);
    let root_heading = spec.get("root_heading").and_then(Json::as_str);
    let Some(root_index) = blocks.iter().position(|b| match root_heading {
        None => true,
        Some(wanted) => heading_matches(&b.heading, wanted),
    }) else {
        findings.push(Finding::new(
            Code::SchemaParse,
            &doc_rel,
            "",
            format!(
                "no JSON block found under heading '{}'",
                root_heading.unwrap_or("None")
            ),
            &doc_rel,
        ));
        return Ok(None);
    };
    let root_block = &blocks[root_index];
    let Some((value, comments)) = parse_block(root_block, &doc_rel, findings) else {
        return Ok(None);
    };
    let mut root = build_node(
        &value,
        &comments,
        &format!("{doc_rel}:L{}", root_block.line),
    );
    if !matches!(value, Json::Object(_)) {
        findings.push(Finding::new(
            Code::SchemaParse,
            &doc_rel,
            &format!("L{}", root_block.line),
            "root example block is not a JSON object".to_string(),
            &doc_rel,
        ));
        return Ok(None);
    }

    let explicit: Vec<(&str, &str)> = match spec.get("sections") {
        None | Some(Json::Null) => Vec::new(),
        Some(Json::Object(members)) => members
            .iter()
            .map(|(heading, path)| {
                path.as_str()
                    .map(|path| (heading.as_str(), path))
                    .ok_or_else(|| "manifest 'sections' must map headings to paths".to_string())
            })
            .collect::<Result<_, _>>()?,
        Some(_) => return Err("manifest field 'sections' must be an object".to_string()),
    };
    let rules: Vec<(&str, &str)> = match spec.get("section_rules") {
        None | Some(Json::Null) => Vec::new(),
        Some(Json::Array(items)) => items
            .iter()
            .map(|rule| {
                Ok((
                    required_str(rule, "heading_regex")?,
                    required_str(rule, "path")?,
                ))
            })
            .collect::<Result<_, String>>()?,
        Some(_) => return Err("manifest field 'section_rules' must be a list".to_string()),
    };
    let ignore = strings(spec, "ignore_headings")?;
    let anchors: BTreeSet<&str> = explicit
        .iter()
        .map(|(heading, _)| *heading)
        .filter(|heading| heading.starts_with("@L"))
        .collect();
    let mut seen_anchors: BTreeSet<&str> = BTreeSet::new();

    let section_path = |env: &mut Env, heading: &str| -> Result<Option<String>, String> {
        if root_heading.is_some_and(|wanted| heading_matches(heading, wanted)) {
            // Another example under the root heading: only a line anchor in
            // the manifest can say what it documents.
            return Ok(None);
        }
        if ignore.iter().any(|ig| heading_matches(heading, ig)) {
            return Ok(None);
        }
        for (name, path) in &explicit {
            if !name.starts_with("@L") && heading_matches(heading, name) {
                return Ok(Some((*path).to_string()));
            }
        }
        for (pattern, path) in &rules {
            if env.validator.regex_search(pattern, heading)? {
                return Ok(Some((*path).to_string()));
            }
        }
        Ok(heading_token(heading))
    };

    for (index, block) in blocks.iter().enumerate() {
        if index == root_index {
            continue;
        }
        let anchor = format!("@L{}", block.line);
        let path = match explicit.iter().find(|(name, _)| *name == anchor) {
            Some(&(name, path)) => {
                seen_anchors.insert(name);
                Some(path.to_string())
            }
            None => section_path(env, &block.heading)?,
        };
        let Some(path) = path else { continue };
        let Some((example, example_comments)) = parse_block(block, &doc_rel, findings) else {
            continue;
        };
        merge_example_at(
            &mut root,
            &path,
            &example,
            &example_comments,
            &format!("{doc_rel}:L{}", block.line),
        );
    }
    // A heading marked `(REQUIRED)` is a contract statement on its own, whether
    // the section body is a JSON block or a table.
    for heading in &headings {
        if !heading.to_uppercase().contains("(REQUIRED)") {
            continue;
        }
        let Some(path) = section_path(env, heading)?.filter(|p| !p.is_empty()) else {
            continue;
        };
        let (parent_path, last) = match path.rsplit_once('.') {
            Some((parent, last)) => (parent, last),
            None => ("", path.as_str()),
        };
        let parent = if parent_path.is_empty() {
            &mut root
        } else {
            walk_path(&mut root, parent_path)
        };
        parent
            .required
            .insert(last.trim_end_matches(['[', ']']).to_string());
    }
    for anchor in anchors.difference(&seen_anchors) {
        findings.push(Finding::new(
            Code::SchemaParse,
            &doc_rel,
            anchor,
            format!(
                "manifest section anchor {anchor} does not point at a JSON block start in this document (the document moved; update artifact-contracts.json)"
            ),
            &doc_rel,
        ));
    }
    // Blocks in OTHER documents that add keys to this artifact.
    if let Some(extras) = spec.get("extra_shapes") {
        let Json::Array(extras) = extras else {
            return Err("manifest field 'extra_shapes' must be a list".to_string());
        };
        for extra in extras {
            let edoc = env.root.join(required_str(extra, "shape")?);
            let edoc_rel = env.rel(&edoc);
            let Some(etext) = read_doc(env, &edoc, "extra_shapes document not found", findings)?
            else {
                continue;
            };
            let heading = required_str(extra, "heading")?;
            let (eblocks, _) = scan_doc(&etext);
            let Some(hit) = eblocks
                .iter()
                .find(|b| heading_matches(&b.heading, heading))
            else {
                findings.push(Finding::new(
                    Code::SchemaParse,
                    &edoc_rel,
                    "",
                    format!("no JSON block under heading '{heading}'"),
                    &edoc_rel,
                ));
                continue;
            };
            let Some((example, example_comments)) = parse_block(hit, &edoc_rel, findings) else {
                continue;
            };
            merge_example_at(
                &mut root,
                required_str(extra, "path")?,
                &example,
                &example_comments,
                &format!("{edoc_rel}:L{}", hit.line),
            );
        }
    }
    for key in strings(spec, "required")? {
        root.required.insert(key.to_string());
    }
    for path in strings(spec, "open_paths")? {
        walk_path(&mut root, path).wildcard = true;
    }
    let allowed: BTreeSet<String> = strings(spec, "allowed_anywhere")?
        .into_iter()
        .map(str::to_string)
        .collect();
    if !allowed.is_empty() {
        mark_allowed_anywhere(&mut root, &allowed);
    }
    Ok(Some(ShapeContract {
        root,
        label: format!("{doc_rel} § {}", root_block.heading),
    }))
}

/// A shape node for a JSON Schema property the Markdown example never showed.
fn node_from_schema(schema: &Json, source: &str) -> Node {
    let mut node = Node {
        source: source.to_string(),
        ..Node::default()
    };
    let Json::Object(_) = schema else {
        node.kinds.insert("any".to_string());
        return node;
    };
    let mut types: Vec<&str> = match schema.get("type") {
        Some(Json::String(name)) => vec![name.as_str()],
        Some(Json::Array(items)) => items.iter().filter_map(Json::as_str).collect(),
        _ => Vec::new(),
    };
    let properties = match schema.get("properties") {
        Some(Json::Object(members)) => Some(members),
        _ => None,
    };
    if types.is_empty() && properties.is_some() {
        types.push("object");
    }
    for name in types {
        match name {
            "integer" => {
                node.kinds.insert("number".to_string());
            }
            "object" | "array" | "string" | "number" | "boolean" | "null" => {
                node.kinds.insert(name.to_string());
            }
            _ => {
                node.kinds.insert("any".to_string());
            }
        }
    }
    if node.kinds.is_empty() {
        node.kinds.insert("any".to_string());
    }
    if let Some(members) = properties {
        node.kinds.insert("object".to_string());
        for (key, sub) in members {
            node.keys.insert(key.clone(), node_from_schema(sub, source));
        }
    }
    if let Some(items @ Json::Object(_)) = schema.get("items") {
        if node.kinds.contains("array") {
            node.item = Some(Box::new(node_from_schema(items, source)));
        }
    }
    node
}

/// Keys the JSON Schema declares and the Markdown shape does not are still in
/// the contract; a key the shape defines is not replaced.
fn admit_schema_properties(node: &mut Node, schema: &Json, source: &str) {
    let Some(Json::Object(properties)) = schema.get("properties") else {
        return;
    };
    if !node.kinds.contains("object") && !node.is_any() {
        return;
    }
    node.kinds.insert("object".to_string());
    for (key, sub) in properties {
        match node.keys.get_mut(key) {
            Some(existing) => admit_schema_properties(existing, sub, source),
            None => {
                node.keys.insert(key.clone(), node_from_schema(sub, source));
            }
        }
    }
}

/// `build_contracts`.
pub(crate) fn build_contracts(
    env: &mut Env,
    manifest: &Json,
    skill: Option<&str>,
    findings: &mut Vec<Finding>,
    include_fixture_only: bool,
) -> Result<Vec<Contract>, String> {
    let mut specs: Vec<(&str, &Json)> = Vec::new();
    if let Some(Json::Object(shared)) = manifest.get("shared") {
        for (glob, spec) in shared {
            if skill.is_some()
                && spec.get("fixture_only").is_some_and(|flag| !is_falsy(flag))
                && !include_fixture_only
            {
                continue;
            }
            specs.push((glob.as_str(), spec));
        }
    }
    if let Some(name) = skill {
        if let Some(Json::Object(own)) = manifest.get("skills").and_then(|s| s.get(name)) {
            for (glob, spec) in own {
                // dict.update: an existing key keeps its position.
                match specs.iter_mut().find(|(existing, _)| *existing == glob) {
                    Some(slot) => slot.1 = spec,
                    None => specs.push((glob.as_str(), spec)),
                }
            }
        }
    }
    let mut contracts = Vec::new();
    for (glob, spec) in specs {
        let mut contract = Contract {
            glob: glob.to_string(),
            shape: None,
            json_schema: None,
            json_schema_path: String::new(),
            routes: Vec::new(),
        };
        if let Some(path) = spec.get("json_schema") {
            let path = path
                .as_str()
                .ok_or_else(|| "manifest field 'json_schema' must be a string".to_string())?;
            let file = env.root.join(path);
            let shown = env.rel(&file);
            let loaded = Origin::Discovered(&env.root)
                .read(&file, &shown)
                .and_then(|text| loads(&text).map_err(|e| e.to_string()));
            match loaded {
                Ok(schema) => {
                    for (pointer, keyword) in unsupported_keywords(&schema, "#") {
                        findings.push(Finding::new(
                            Code::SchemaParse,
                            &shown,
                            &pointer,
                            format!(
                                "JSON Schema keyword '{keyword}' is not implemented by this validator's subset — it would be skipped silently. Add it to ENFORCED (with a check) or ANNOTATIONS (if it carries no constraint) in startup/artifacts/schema.rs."
                            ),
                            &shown,
                        ));
                    }
                    contract.json_schema = Some(schema);
                    contract.json_schema_path = shown;
                }
                Err(reason) => findings.push(Finding::new(
                    Code::SchemaParse,
                    &shown,
                    "",
                    format!("cannot load JSON Schema: {reason}"),
                    &shown,
                )),
            }
        }
        if spec.get("shape").is_some() {
            contract.shape = build_shape_contract(env, spec, findings)?;
        }
        if let (Some(shape), Some(schema)) =
            (contract.shape.as_mut(), contract.json_schema.as_ref())
        {
            admit_schema_properties(&mut shape.root, schema, &contract.json_schema_path);
        }
        if let Some(routes) = spec.get("route_shapes") {
            let Json::Array(routes) = routes else {
                return Err("manifest field 'route_shapes' must be a list".to_string());
            };
            for route in routes {
                if let Some(built) = build_shape_contract(env, route, findings)? {
                    contract.routes.push((
                        required_str(route, "when_path")?.to_string(),
                        route.get("when_value").cloned().unwrap_or(Json::Null),
                        built,
                    ));
                }
            }
        }
        contracts.push(contract);
    }
    Ok(contracts)
}

/// `_value_at`: the value at a dotted path, `null` when anything is missing.
static NULL: Json = Json::Null;

fn value_at<'a>(value: &'a Json, dotted: &str) -> &'a Json {
    let mut current = value;
    for part in dotted.split('.') {
        match current.get(part) {
            Some(next) => current = next,
            None => return &NULL,
        }
    }
    current
}

/// The shape that applies to `value`: the first route whose marker matches,
/// else the contract's default.
pub(crate) fn shape_for<'a>(contract: &'a Contract, value: &Json) -> Option<&'a ShapeContract> {
    contract
        .routes
        .iter()
        .find(|(path, expected, _)| py_eq(value_at(value, path), expected))
        .map(|(_, _, route)| route)
        .or(contract.shape.as_ref())
}

/// `artifact_name`: `scenarios/scenario-001.preferences.json` becomes
/// `scenarios/scenario-NNN.preferences.json`.
pub(crate) fn artifact_name(path: &Path) -> String {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let chars: Vec<char> = name.chars().collect();
    let marker: Vec<char> = "scenario-".chars().collect();
    let mut normalised = String::new();
    let mut at = 0;
    while at < chars.len() {
        if chars[at..].starts_with(&marker) {
            let digits_from = at + marker.len();
            let digits = chars[digits_from..]
                .iter()
                .take_while(|c| c.is_ascii_digit())
                .count();
            if digits > 0 && chars.get(digits_from + digits) == Some(&'.') {
                normalised.push_str("scenario-NNN.");
                at = digits_from + digits + 1;
                continue;
            }
        }
        normalised.push(chars[at]);
        at += 1;
    }
    let in_scenarios = path
        .parent()
        .and_then(Path::file_name)
        .is_some_and(|parent| parent == "scenarios");
    if in_scenarios {
        format!("scenarios/{normalised}")
    } else {
        normalised
    }
}

/// Whether any manifest glob matches the artifact name.
pub(crate) fn matches_glob(name: &str, contract: &Contract) -> bool {
    fnmatch(name, &contract.glob)
}

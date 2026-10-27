//! Key-path templates inferred from contract examples, and the artifact check
//! against them (`Node`, `build_node`, `validate_shape` in the Python tool).

use std::collections::{BTreeMap, BTreeSet, HashMap};

use toolkit_core::Json;

use super::patterns::{
    enum_from_comment, enum_from_string, is_enum_token, is_placeholder, mentions_joined_set,
};
use super::{Code, Finding};

/// Comments by the key they annotate.
pub(crate) type KeyComments = HashMap<String, Vec<String>>;

/// One template node. Sets are ordered so findings are deterministic (the
/// Python tool iterated hash sets).
#[derive(Clone, Debug, Default)]
pub(crate) struct Node {
    pub(crate) kinds: BTreeSet<String>,
    pub(crate) keys: BTreeMap<String, Node>,
    pub(crate) required: BTreeSet<String>,
    pub(crate) wildcard: bool,
    pub(crate) any_key: Option<Box<Node>>,
    pub(crate) item: Option<Box<Node>>,
    pub(crate) enum_values: Option<BTreeSet<String>>,
    pub(crate) enum_join: Option<String>,
    pub(crate) examples: BTreeSet<String>,
    pub(crate) source: String,
}

fn kinds_of(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|name| (*name).to_string()).collect()
}

impl Node {
    pub(crate) fn with_kinds(names: &[&str], source: &str) -> Self {
        Self {
            kinds: kinds_of(names),
            source: source.to_string(),
            ..Self::default()
        }
    }

    pub(crate) fn is_any(&self) -> bool {
        self.kinds.contains("any") || self.kinds.is_empty()
    }

    /// An empty-array seed or a bare placeholder: no type, keys or enum.
    fn is_unspecified_any(&self) -> bool {
        self.kinds == kinds_of(&["any"])
            && self.keys.is_empty()
            && self.item.is_none()
            && self.any_key.is_none()
            && self.enum_values.is_none()
            && self.examples.is_empty()
            && !self.wildcard
            && self.required.is_empty()
    }

    /// Unions two templates for the same path. Children adopted from `other`
    /// are copies, never shared.
    pub(crate) fn merge(&mut self, other: &Node) {
        if self.is_unspecified_any() && !other.is_unspecified_any() {
            let source = if other.source.is_empty() {
                self.source.clone()
            } else {
                other.source.clone()
            };
            *self = other.clone();
            self.source = source;
            return;
        }
        if other.is_unspecified_any() && !self.is_unspecified_any() {
            return;
        }
        self.kinds.extend(other.kinds.iter().cloned());
        self.wildcard = self.wildcard || other.wildcard;
        self.required.extend(other.required.iter().cloned());
        for (key, value) in &other.keys {
            match self.keys.get_mut(key) {
                Some(existing) => existing.merge(value),
                None => {
                    self.keys.insert(key.clone(), value.clone());
                }
            }
        }
        merge_optional(&mut self.item, other.item.as_deref());
        merge_optional(&mut self.any_key, other.any_key.as_deref());
        if self.enum_join.is_none() {
            self.enum_join.clone_from(&other.enum_join);
        }
        self.examples.extend(other.examples.iter().cloned());
        if self.enum_values.is_some() || other.enum_values.is_some() {
            // A declared enum survives a merge with a concrete example; a
            // token the document shows joins the set, free text proves the
            // field is not an enum.
            if self.examples.iter().any(|e| !is_enum_token(e)) {
                self.enum_values = None;
            } else {
                let mut merged: BTreeSet<String> = self.enum_values.clone().unwrap_or_default();
                merged.extend(other.enum_values.iter().flatten().cloned());
                merged.extend(self.examples.iter().cloned());
                self.enum_values = Some(merged);
            }
        }
    }
}

fn merge_optional(target: &mut Option<Box<Node>>, other: Option<&Node>) {
    let Some(other) = other else { return };
    match target {
        Some(existing) => existing.merge(other),
        None => *target = Some(Box::new(other.clone())),
    }
}

/// `_is_required_comment`.
fn is_required_comment(comment: &str) -> bool {
    let up = comment.to_uppercase();
    (up.contains("REQUIRED")
        && !up.contains("NOT REQUIRED")
        && !up.contains("REQUIRED WHEN")
        && !up.contains("REQUIRED IF")
        && !up.contains("REQUIRED ONLY")
        && !up.contains("REQUIRED ON")
        && !up.contains("REQUIRED WHENEVER"))
        || up.contains("ALWAYS PRESENT")
}

/// Infers a template node from an example value.
pub(crate) fn build_node(value: &Json, key_comments: &KeyComments, source: &str) -> Node {
    let mut node = Node {
        source: source.to_string(),
        ..Node::default()
    };
    match value {
        Json::Object(members) => {
            node.kinds.insert("object".to_string());
            for (key, member) in members {
                if key == "..." || is_placeholder(key) {
                    node.wildcard = true;
                    let nonempty = match member {
                        Json::Object(inner) => {
                            !inner.is_empty()
                                && !(inner.len() == 1 && inner.iter().any(|(k, _)| k == "..."))
                        }
                        Json::Array(inner) => !inner.is_empty(),
                        _ => false,
                    };
                    if nonempty {
                        let template = build_node(member, key_comments, source);
                        merge_optional(&mut node.any_key, Some(&template));
                    }
                    continue;
                }
                let mut child = build_node(member, key_comments, source);
                for comment in key_comments.get(key).into_iter().flatten() {
                    if is_required_comment(comment) {
                        node.required.insert(key.clone());
                    }
                    apply_comment(&mut child, member, comment);
                }
                node.keys.insert(key.clone(), child);
            }
        }
        Json::Array(items) => {
            node.kinds.insert("array".to_string());
            let mut item: Option<Node> = None;
            for element in items {
                let built = build_node(element, key_comments, source);
                match &mut item {
                    Some(existing) => existing.merge(&built),
                    None => item = Some(built),
                }
            }
            node.item = Some(Box::new(
                item.unwrap_or_else(|| Node::with_kinds(&["any"], source)),
            ));
        }
        Json::String(text) => {
            if text == "..." || is_placeholder(text) {
                node.kinds.insert("any".to_string());
            } else {
                node.kinds.insert("string".to_string());
                match enum_from_string(text) {
                    Some(values) => node.enum_values = Some(values),
                    None => {
                        node.examples.insert(text.clone());
                    }
                }
            }
        }
        Json::Bool(_) => {
            node.kinds.insert("boolean".to_string());
        }
        Json::Int(_) | Json::Float(_) => {
            node.kinds.insert("number".to_string());
        }
        Json::Null => {
            node.kinds.insert("null".to_string());
            node.kinds.insert("any".to_string());
        }
    }
    node
}

/// A comment next to a key can declare an enum and nullability.
fn apply_comment(child: &mut Node, value: &Json, comment: &str) {
    if let Some(values) = enum_from_comment(comment) {
        if child.kinds.contains("string") || child.kinds.contains("null") || child.is_any() {
            // Only trust a comment enum when the example agrees with it.
            let consistent = match value {
                Json::String(text) => {
                    values.contains(text) || is_placeholder(text) || text == "..."
                }
                _ => true,
            };
            if consistent {
                child.enum_values = Some(values);
                child.kinds.insert("string".to_string());
                child.kinds.remove("any");
                if mentions_joined_set(comment) {
                    child.enum_join = Some("+".to_string());
                }
            }
        }
    }
    let lower = comment.to_lowercase();
    if lower.contains("or null") || lower.contains("nullable") {
        child.kinds.insert("null".to_string());
    }
}

/// The node at `a.b[].c`, creating intermediate nodes.
pub(crate) fn walk_path<'a>(root: &'a mut Node, dotted: &str) -> &'a mut Node {
    let source = root.source.clone();
    let mut node = root;
    if dotted.is_empty() {
        return node;
    }
    for part in dotted.split('.') {
        let is_array = part.ends_with("[]");
        let key = if is_array {
            &part[..part.len() - 2]
        } else {
            part
        };
        if !key.is_empty() {
            let template = node.any_key.as_deref().cloned();
            node = node.keys.entry(key.to_string()).or_insert_with(|| {
                let mut fresh =
                    Node::with_kinds(&[if is_array { "array" } else { "object" }], &source);
                if let Some(template) = &template {
                    fresh.merge(template);
                }
                fresh
            });
        }
        if is_array {
            node.kinds.insert("array".to_string());
            let boxed = node
                .item
                .get_or_insert_with(|| Box::new(Node::with_kinds(&["object"], &source)));
            node = boxed.as_mut();
        }
    }
    node
}

/// Peels single-key wrapper objects that spell out the heading's key path.
fn unwrap_to_path<'a>(mut value: &'a Json, dotted: &str) -> &'a Json {
    let components: Vec<&str> = dotted
        .split('.')
        .filter(|part| !part.is_empty())
        .map(|part| part.trim_end_matches(['[', ']']))
        .collect();
    for component in &components {
        match value {
            Json::Object(members) if members.len() == 1 && members[0].0 == *component => {
                value = &members[0].1;
            }
            _ => break,
        }
    }
    if let (Some(last), Json::Object(members)) = (components.last(), value) {
        if members.len() == 1
            && members[0].0 == *last
            && matches!(members[0].1, Json::Object(_) | Json::Array(_))
        {
            value = &members[0].1;
        }
    }
    value
}

/// Merges an example block into the template at `path`, at the level the
/// example actually shows.
pub(crate) fn merge_example_at(
    root: &mut Node,
    path: &str,
    value: &Json,
    key_comments: &KeyComments,
    source: &str,
) {
    let value = unwrap_to_path(value, path);
    let node = build_node(value, key_comments, source);
    if path.ends_with("[]") && matches!(value, Json::Array(_)) {
        let target = walk_path(root, &path[..path.len() - 2]);
        // The fresh leaf `[]` stripping created is an object; the example is
        // the array itself.
        if target.kinds == kinds_of(&["object"])
            && target.keys.is_empty()
            && target.item.is_none()
            && target.any_key.is_none()
        {
            target.kinds = kinds_of(&["array"]);
        }
        target.merge(&node);
    } else {
        walk_path(root, path).merge(&node);
    }
}

/// Keys a document defines as a vocabulary usable on any row.
pub(crate) fn mark_allowed_anywhere(node: &mut Node, allowed: &BTreeSet<String>) {
    if node.kinds.contains("object") && !node.keys.is_empty() {
        let source = node.source.clone();
        for key in allowed {
            node.keys
                .entry(key.clone())
                .or_insert_with(|| Node::with_kinds(&["any"], &source));
        }
        for child in node.keys.values_mut() {
            mark_allowed_anywhere(child, allowed);
        }
    }
    if let Some(item) = node.item.as_deref_mut() {
        mark_allowed_anywhere(item, allowed);
    }
}

pub(crate) fn join_path(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_string()
    } else {
        format!("{path}.{key}")
    }
}

fn kind_of(value: &Json) -> &'static str {
    match value {
        Json::Object(_) => "object",
        Json::Array(_) => "array",
        Json::String(_) => "string",
        Json::Bool(_) => "boolean",
        Json::Int(_) | Json::Float(_) => "number",
        Json::Null => "null",
    }
}

/// Checks `value` against the template, appending findings.
pub(crate) fn validate_shape(
    node: &Node,
    value: &Json,
    path: &str,
    artifact: &str,
    contract: &str,
    out: &mut Vec<Finding>,
) {
    let kind = kind_of(value);
    let shown = |path: &str| {
        if path.is_empty() {
            "$".to_string()
        } else {
            path.to_string()
        }
    };
    if !node.is_any() && !node.kinds.contains(kind) {
        // null is tolerated where the example never said.
        if kind == "null" {
            return;
        }
        let expected: Vec<&str> = node.kinds.iter().map(String::as_str).collect();
        out.push(Finding::new(
            Code::TypeMismatch,
            artifact,
            &shown(path),
            format!("is {kind}, contract example is {}", expected.join("/")),
            contract,
        ));
        return;
    }
    if let (Some(values), Json::String(text)) = (&node.enum_values, value) {
        // A joined set (`terraform+live`) is valid when every member is.
        let members: Vec<&str> = match &node.enum_join {
            Some(separator) => text.split(separator.as_str()).collect(),
            None => vec![text.as_str()],
        };
        if members.iter().any(|member| !values.contains(*member)) {
            let allowed: Vec<&str> = values.iter().map(String::as_str).collect();
            let joined = node
                .enum_join
                .as_ref()
                .map_or_else(String::new, |separator| {
                    format!(" (or a '{separator}'-joined set of them)")
                });
            out.push(Finding::new(
                Code::EnumViolation,
                artifact,
                &shown(path),
                format!("'{text}' not in {{{}}}{joined}", allowed.join(", ")),
                contract,
            ));
        }
    }
    match value {
        Json::Object(members) if node.kinds.contains("object") => {
            for key in &node.required {
                if !members.iter().any(|(name, _)| name == key) {
                    out.push(Finding::new(
                        Code::MissingRequired,
                        artifact,
                        &join_path(path, key),
                        "contract marks this key REQUIRED / ALWAYS present".to_string(),
                        contract,
                    ));
                }
            }
            for (key, member) in members {
                if key.starts_with('_') {
                    continue;
                }
                match node.keys.get(key) {
                    Some(child) => {
                        validate_shape(
                            child,
                            member,
                            &join_path(path, key),
                            artifact,
                            contract,
                            out,
                        );
                    }
                    None => {
                        if let Some(template) = node.any_key.as_deref() {
                            validate_shape(
                                template,
                                member,
                                &join_path(path, key),
                                artifact,
                                contract,
                                out,
                            );
                        } else if !node.wildcard && !node.keys.is_empty() {
                            out.push(Finding::new(
                                Code::UnknownKey,
                                artifact,
                                &join_path(path, key),
                                "key is not in the contract's example shape".to_string(),
                                contract,
                            ));
                        }
                    }
                }
            }
        }
        Json::Array(items) if node.kinds.contains("array") => {
            if let Some(item) = node.item.as_deref() {
                for (index, element) in items.iter().enumerate() {
                    validate_shape(
                        item,
                        element,
                        &format!("{path}[{index}]"),
                        artifact,
                        contract,
                        out,
                    );
                }
            }
        }
        _ => {}
    }
}

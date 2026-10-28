//! Shared helpers of the schema-driven AI contract ports (#4806): fixture and
//! schema loading, validation through the crate's schema-subset validator
//! (the Rust `validate_json_schema`), JSON navigation and mutation, and the
//! scratch copies the contracts are shown to fail on.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use toolkit_core::Json;

use super::documents::{node, put};
use super::{copy_dir, json, real_plugin, TempDir};
use crate::startup::artifacts::pyjson::dumps;
use crate::startup::artifacts::schema::{unsupported_keywords, Context, SubsetValidator};

/// A file of the plugin tree, by plugin-relative path.
pub(super) fn read(rel: &str) -> String {
    fs::read_to_string(real_plugin().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

/// A JSON file of the plugin tree.
pub(super) fn load(rel: &str) -> Json {
    json(&read(rel))
}

/// A JSON file below `root`.
pub(super) fn load_at(root: &Path, rel: &str) -> Json {
    let path = root.join(rel);
    json(&fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())))
}

/// `validate_json_schema(schema, instance, "", name, schema_rel, findings)`:
/// the `(path, message)` pairs of the findings.
pub(super) fn findings(schema_rel: &str, instance: &Json) -> Vec<(String, String)> {
    let schema = load(schema_rel);
    let mut validator = SubsetValidator::default();
    let mut out = Vec::new();
    let context = Context {
        artifact: "instance",
        contract: schema_rel,
    };
    validator
        .validate(&schema, instance, "", &context, &mut out, &schema)
        .expect("the schema evaluates");
    out.into_iter().map(|f| (f.path, f.message)).collect()
}

/// `unsupported_keywords(schema) == []`.
pub(super) fn schema_is_in_the_supported_subset(schema_rel: &str) -> bool {
    unsupported_keywords(&load(schema_rel), "").is_empty()
}

/// The shape of a draft-07 object schema (`$schema` and `type`).
pub(super) fn draft07_object_shaped(schema: &Json) -> bool {
    schema.get("$schema").and_then(Json::as_str) == Some("http://json-schema.org/draft-07/schema#")
        && schema.get("type").and_then(Json::as_str) == Some("object")
}

/// `test_schema_has_no_unsupported_keywords`, with its non-vacuity: a schema
/// using `contains` is reported.
pub(super) fn assert_no_unsupported_keywords(schema_rel: &str) {
    assert!(
        schema_is_in_the_supported_subset(schema_rel),
        "{schema_rel} uses a keyword the validator does not enforce"
    );
    let mut with_contains = load(schema_rel);
    put(&mut with_contains, "", "contains", Json::object(Vec::new()));
    assert!(!unsupported_keywords(&with_contains, "").is_empty());
}

/// `test_schema_is_valid_json_and_draft07_shaped`, with its non-vacuity: a
/// schema of another draft or another root type is refused.
pub(super) fn assert_draft07_shaped(schema_rel: &str) {
    let schema = load(schema_rel);
    assert!(draft07_object_shaped(&schema), "{schema_rel}");
    let mut wrong_draft = schema.clone();
    put(&mut wrong_draft, "", "$schema", Json::string("other"));
    assert!(!draft07_object_shaped(&wrong_draft));
    let mut wrong_type = schema;
    put(&mut wrong_type, "", "type", Json::string("array"));
    assert!(!draft07_object_shaped(&wrong_type));
}

/// The sorted `<dir>/<name>` fixtures below `root_rel` (Python
/// `sorted(root.glob("*/<name>"))`): the directory name and the parsed file.
pub(super) fn fixtures_named(root_rel: &str, name: &str) -> Vec<(String, Json)> {
    let root = real_plugin().join(root_rel);
    let mut dirs: Vec<String> = fs::read_dir(&root)
        .unwrap_or_else(|e| panic!("{root_rel}: {e}"))
        .map(|entry| entry.expect("dir entry"))
        .filter(|entry| entry.path().join(name).is_file())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    dirs.sort();
    assert!(!dirs.is_empty(), "no {name} below {root_rel}");
    dirs.into_iter()
        .map(|dir| {
            let value = load(&format!("{root_rel}/{dir}/{name}"));
            (dir, value)
        })
        .collect()
}

/// A clone of `base` edited by `edit`.
pub(super) fn edited(base: &Json, edit: impl FnOnce(&mut Json)) -> Json {
    let mut copy = base.clone();
    edit(&mut copy);
    copy
}

/// Asserts that the schema accepts `base` and rejects its edited copy: the
/// negative mutations of the Python files, each tied to an accepted control.
pub(super) fn assert_rejected(schema_rel: &str, base: &Json, edit: impl FnOnce(&mut Json)) {
    assert!(
        findings(schema_rel, base).is_empty(),
        "the unedited document must be accepted: {:?}",
        findings(schema_rel, base)
    );
    let broken = edited(base, edit);
    assert!(
        !findings(schema_rel, &broken).is_empty(),
        "the edited document is still accepted by {schema_rel}"
    );
}

// ---- JSON navigation ---------------------------------------------------------------

/// The value at a dotted path (array indexes are numbers; `""` is the root).
pub(super) fn at<'a>(value: &'a Json, path: &str) -> &'a Json {
    let mut current = value;
    for part in path.split('.').filter(|part| !part.is_empty()) {
        current = match current {
            Json::Object(_) => current
                .get(part)
                .unwrap_or_else(|| panic!("no key {part} in {path}")),
            Json::Array(items) => &items[part.parse::<usize>().expect("array index")],
            other => panic!("cannot descend into {other:?} at {part}"),
        };
    }
    current
}

/// The array at `path`.
pub(super) fn items<'a>(value: &'a Json, path: &str) -> &'a [Json] {
    match at(value, path) {
        Json::Array(items) => items,
        other => panic!("{path} is not an array: {other:?}"),
    }
}

/// The string at `path`.
pub(super) fn text_at<'a>(value: &'a Json, path: &str) -> &'a str {
    at(value, path)
        .as_str()
        .unwrap_or_else(|| panic!("{path} is not a string"))
}

/// The string member `key` of an object.
pub(super) fn str_of<'a>(value: &'a Json, key: &str) -> &'a str {
    value
        .get(key)
        .and_then(Json::as_str)
        .unwrap_or_else(|| panic!("no string member {key}"))
}

/// Whether the object has the member `key`.
pub(super) fn has_key(value: &Json, key: &str) -> bool {
    value.get(key).is_some()
}

/// The set of strings in the array `value`.
pub(super) fn string_set(value: &Json) -> BTreeSet<String> {
    match value {
        Json::Array(items) => items
            .iter()
            .map(|item| item.as_str().expect("a string").to_owned())
            .collect(),
        other => panic!("not an array: {other:?}"),
    }
}

/// The strings of the array `value`, in order.
pub(super) fn string_list(value: &Json) -> Vec<String> {
    match value {
        Json::Array(items) => items
            .iter()
            .map(|item| item.as_str().expect("a string").to_owned())
            .collect(),
        other => panic!("not an array: {other:?}"),
    }
}

/// A JSON array of strings.
pub(super) fn strings(items: &[&str]) -> Json {
    Json::Array(items.iter().map(|item| Json::string(*item)).collect())
}

/// `a == b` as Python compares dicts: object key order does not matter.
pub(super) fn same(a: &Json, b: &Json) -> bool {
    canonical(a) == canonical(b)
}

fn canonical(value: &Json) -> Json {
    match value {
        Json::Object(members) => {
            let mut sorted: Vec<(String, Json)> = members
                .iter()
                .map(|(key, inner)| (key.clone(), canonical(inner)))
                .collect();
            sorted.sort_by(|left, right| left.0.cmp(&right.0));
            Json::Object(sorted)
        }
        Json::Array(items) => Json::Array(items.iter().map(canonical).collect()),
        other => other.clone(),
    }
}

// ---- JSON mutation -------------------------------------------------------------------

/// `value[path].append(item)`.
pub(super) fn push(value: &mut Json, path: &str, item: Json) {
    match node(value, path) {
        Json::Array(items) => items.push(item),
        other => panic!("{path} is not an array: {other:?}"),
    }
}

/// `value[path].remove(text)` for an array of strings.
pub(super) fn remove_string(value: &mut Json, path: &str, text: &str) {
    match node(value, path) {
        Json::Array(items) => {
            let position = items
                .iter()
                .position(|item| item.as_str() == Some(text))
                .unwrap_or_else(|| panic!("{text:?} is not in {path}"));
            items.remove(position);
        }
        other => panic!("{path} is not an array: {other:?}"),
    }
}

/// `value[path] = new` for the last path segment (an object member or an
/// array index).
pub(super) fn assign(value: &mut Json, path: &str, new: Json) {
    let (parent, last) = path.rsplit_once('.').unwrap_or(("", path));
    match node(value, parent) {
        Json::Array(items) => items[last.parse::<usize>().expect("array index")] = new,
        _ => put(value, parent, last, new),
    }
}

// ---- scratch copies of fixture trees -------------------------------------------------

/// A scratch copy of a fixture directory of the plugin.
pub(super) struct Tree {
    dir: TempDir,
    root: PathBuf,
}

impl Tree {
    pub(super) fn of(root_rel: &str, label: &str) -> Self {
        let dir = TempDir::new(label);
        let root = dir.join("copy");
        copy_dir(&real_plugin().join(root_rel), &root);
        Self { dir, root }
    }

    /// The copied tree.
    pub(super) fn root(&self) -> &Path {
        let _ = &self.dir;
        &self.root
    }

    /// Rewrites the JSON file `rel` of the copy with `edit` applied.
    pub(super) fn edit_json(&self, rel: &str, edit: impl FnOnce(&mut Json)) {
        let mut value = load_at(&self.root, rel);
        edit(&mut value);
        fs::write(self.root.join(rel), dumps(&value, Some(2), false)).expect("write edited copy");
    }

    /// Removes the file `rel` of the copy.
    pub(super) fn remove(&self, rel: &str) {
        fs::remove_file(self.root.join(rel)).expect("remove from the copy");
    }

    /// Writes the file `rel` of the copy.
    pub(super) fn write(&self, rel: &str, content: &str) {
        let path = self.root.join(rel);
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(path, content).expect("write into the copy");
    }
}

impl Tree {
    /// A scratch plugin root holding only the subtree `rel` of the real plugin.
    pub(super) fn of_subtree(rel: &str, label: &str) -> Self {
        let dir = TempDir::new(label);
        let root = dir.join("plugin");
        copy_dir(&real_plugin().join(rel), &root.join(rel));
        Self { dir, root }
    }
}

/// `tools/sync-vendored.py --check`: every vendored copy of the plugin is byte-identical to its
/// canonical file (the Rust `toolkit sync vendored --check`), shown to fail on a scratch copy
/// whose vendored file has drifted.
pub(super) fn assert_vendored_copies_are_byte_identical() {
    let report =
        toolkit_core::sync_vendored(&real_plugin(), true).expect("the vendored check runs");
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert!(report.total_files > 0, "no vendored file was checked");

    let tree = Tree::of_subtree("skills", "vendored-drift");
    let drifted = tree
        .root()
        .join("skills/gcp-to-aws/references/vendored/ai/ai-capability-catalog.json");
    let mut bytes = fs::read(&drifted).expect("the vendored catalog");
    bytes.push(b' ');
    fs::write(&drifted, bytes).expect("drift the copy");
    let report = toolkit_core::sync_vendored(tree.root(), true).expect("the vendored check runs");
    assert!(
        !report.errors.is_empty(),
        "a drifted copy passes the vendored check"
    );
}

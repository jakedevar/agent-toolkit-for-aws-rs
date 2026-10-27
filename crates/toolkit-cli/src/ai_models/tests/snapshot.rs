use std::fs;
use std::path::Path;

use toolkit_core::Json;

use super::{catalog, dumps, j};
use super::{write, TempDir};
use crate::ai_models::snapshot::{
    build_catalog_snapshot, create_catalog_snapshot_file, load_catalog_snapshot,
    validate_catalog_snapshot, write_catalog_snapshot, SnapshotError,
};

const HUB: &str = "SageMakerPublicHub";
const WHEN: &str = "2024-07-01T12:00:00+00:00";

fn built() -> Json {
    build_catalog_snapshot(HUB, Some("us-east-2"), &catalog(), WHEN).expect("builds")
}

fn rejected(snapshot: &Json) -> String {
    validate_catalog_snapshot(snapshot, None, None)
        .expect_err("rejected")
        .0
}

/// `snapshot` with one member replaced.
#[allow(clippy::needless_pass_by_value)]
fn with(snapshot: &Json, key: &str, value: Json) -> Json {
    let Json::Object(members) = snapshot else {
        panic!("object")
    };
    Json::Object(
        members
            .iter()
            .map(|(name, old)| {
                (
                    name.clone(),
                    if name == key {
                        value.clone()
                    } else {
                        old.clone()
                    },
                )
            })
            .collect(),
    )
}

#[test]
fn a_snapshot_has_the_python_members_in_order() {
    let snapshot = built();
    let Json::Object(members) = &snapshot else {
        panic!("object")
    };
    let keys: Vec<&str> = members.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(
        keys,
        [
            "schema_version",
            "hub_name",
            "region",
            "fetched_at",
            "model_count",
            "available_values",
            "raw_sizes_by_bucket",
            "original_creation_times",
            "catalog_models"
        ]
    );
    assert_eq!(snapshot.get("model_count"), Some(&Json::Int(4)));
    assert_eq!(
        dumps(snapshot.get("raw_sizes_by_bucket").expect("sizes")),
        r#"{"<=10b": ["7b", "8B", "<1b"], "11b-70b": ["10b-70b"], "71b-100b": [], ">100b": [], "unknown": []}"#
    );
    assert_eq!(
        dumps(snapshot.get("original_creation_times").expect("times")),
        concat!(
            r#"{"llama-3-8b": "2024-05-01T00:00:00+00:00", "Mistral-7B": "2024-06-01T00:00:00+00:00", "#,
            r#""tiny-embedder": "2023-01-01T00:00:00+00:00"}"#
        )
    );
    validate_catalog_snapshot(&snapshot, Some(HUB), Some("us-east-2")).expect("valid");
}

#[test]
fn building_requires_a_region_a_hub_a_time_and_object_models() {
    let build = |hub: &str, region: Option<&str>, models: &[Json], when: &str| {
        build_catalog_snapshot(hub, region, models, when)
            .expect_err("rejected")
            .0
    };
    assert_eq!(
        build("", Some("us-east-1"), &[], WHEN),
        "Snapshot hub_name must be a non-empty string"
    );
    for region in [None, Some("")] {
        assert_eq!(
            build(HUB, region, &[], WHEN),
            "A configured AWS region is required to create a catalog snapshot"
        );
    }
    assert_eq!(
        build(HUB, Some("us-east-1"), &[Json::Int(1)], WHEN),
        "Snapshot catalog_models must be a list of objects"
    );
    assert_eq!(
        build(HUB, Some("us-east-1"), &[], ""),
        "Snapshot fetched_at must be a non-empty timestamp"
    );
}

#[test]
fn validation_names_the_first_broken_rule() {
    let good = built();
    let cases: Vec<(Json, &str)> = vec![
        (
            Json::Array(vec![]),
            "Catalog snapshot must be a JSON object",
        ),
        (
            with(&good, "schema_version", Json::Int(2)),
            "Unsupported catalog snapshot schema: 2",
        ),
        (
            with(&good, "schema_version", Json::string("1")),
            "Unsupported catalog snapshot schema: \"1\"",
        ),
        (
            with(&good, "hub_name", Json::string("")),
            "Catalog snapshot hub_name must be a non-empty string",
        ),
        (
            with(&good, "region", Json::Int(5)),
            "Catalog snapshot region must be a non-empty string",
        ),
        (
            with(&good, "fetched_at", Json::Null),
            "Catalog snapshot fetched_at must be a timestamp string",
        ),
        (
            with(&good, "catalog_models", Json::string("x")),
            "Catalog snapshot catalog_models must be a list of objects",
        ),
        (
            with(&good, "catalog_models", j("[1]")),
            "Catalog snapshot catalog_models must be a list of objects",
        ),
        (
            with(&good, "model_count", Json::string("4")),
            "Catalog snapshot model_count must be an integer",
        ),
        (
            with(&good, "model_count", Json::Bool(true)),
            "Catalog snapshot model_count must be an integer",
        ),
        (
            with(&good, "model_count", Json::Int(3)),
            "Catalog snapshot model_count does not match catalog_models length",
        ),
        (
            with(&good, "available_values", Json::Null),
            "Catalog snapshot available_values must be an object",
        ),
        (
            with(&good, "raw_sizes_by_bucket", Json::Array(vec![])),
            "Catalog snapshot raw_sizes_by_bucket must be an object",
        ),
        (
            with(&good, "original_creation_times", Json::Int(1)),
            "Catalog snapshot original_creation_times must be an object",
        ),
    ];
    for (snapshot, message) in cases {
        assert_eq!(rejected(&snapshot), message);
    }
}

#[test]
fn model_names_must_be_present_and_unique() {
    let good = built();
    let nameless = with(
        &good,
        "catalog_models",
        j(r#"[{"name": "a"}, {"size": "x"}]"#),
    );
    let nameless = with(&nameless, "model_count", Json::Int(2));
    assert_eq!(
        rejected(&nameless),
        "Every cached model must have a non-empty name"
    );
    let twice = with(
        &good,
        "catalog_models",
        j(r#"[{"name": "a"}, {"name": "a"}]"#),
    );
    let twice = with(&twice, "model_count", Json::Int(2));
    assert_eq!(
        rejected(&twice),
        "Catalog snapshot contains duplicate model names"
    );
}

#[test]
fn a_mismatched_hub_or_region_is_refused_without_forging_a_line() {
    let hostile = "evil\u{1b}[31m\u{202e}\nerror: injected";
    let snapshot = with(&built(), "hub_name", Json::string(hostile));
    let message = validate_catalog_snapshot(&snapshot, Some(HUB), None)
        .expect_err("mismatch")
        .0;
    assert!(
        message.starts_with("Catalog snapshot is for Hub \"evil"),
        "{message}"
    );
    assert!(message.ends_with(&format!(", not \"{HUB}\"")), "{message}");
    assert!(
        !message.contains('\u{1b}') && !message.contains('\u{202e}'),
        "{message}"
    );
    assert!(!message.contains('\n'), "{message}");
    let region =
        validate_catalog_snapshot(&built(), None, Some("eu-west-1")).expect_err("mismatch");
    assert_eq!(
        region,
        SnapshotError(
            "Catalog snapshot is for region \"us-east-2\", not \"eu-west-1\"".to_string()
        )
    );
}

#[test]
fn a_written_snapshot_round_trips_and_is_private() {
    let dir = TempDir::new("ai-models-snapshot-write");
    let path = dir.join("catalog.json");
    let snapshot = built();
    write_catalog_snapshot(&path, &snapshot).expect("writes");
    assert_eq!(fs::read_to_string(&path).expect("read"), dumps(&snapshot));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&path).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
    assert_eq!(
        load_catalog_snapshot(&path, Some(HUB), Some("us-east-2")).expect("loads"),
        snapshot
    );
}

#[cfg(unix)]
#[test]
fn an_existing_world_readable_path_is_restricted_and_truncated() {
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new("ai-models-snapshot-existing");
    let path = dir.join("catalog.json");
    fs::write(&path, "x".repeat(100_000)).expect("seed");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("chmod");
    write_catalog_snapshot(&path, &built()).expect("writes");
    let mode = fs::metadata(&path).expect("meta").permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    assert_eq!(fs::read_to_string(&path).expect("read"), dumps(&built()));
}

#[cfg(unix)]
#[test]
fn symbolic_links_are_refused_for_writes_and_reads() {
    let dir = TempDir::new("ai-models-snapshot-link");
    let target = dir.join("target.json");
    fs::write(&target, "keep").expect("seed");
    let link = dir.join("link.json");
    std::os::unix::fs::symlink(&target, &link).expect("symlink");
    let error = write_catalog_snapshot(&link, &built()).expect_err("refused");
    assert_eq!(
        error.0,
        "Could not write catalog snapshot: symbolic links are not followed"
    );
    assert_eq!(fs::read_to_string(&target).expect("read"), "keep");
    // A dangling link is refused as well.
    let dangling = dir.join("dangling.json");
    std::os::unix::fs::symlink(dir.join("nowhere"), &dangling).expect("symlink");
    assert!(write_catalog_snapshot(&dangling, &built()).is_err());
    assert!(!dir.join("nowhere").exists());
    let read = load_catalog_snapshot(&link, None, None).expect_err("refused");
    assert_eq!(
        read.0,
        "Could not read catalog snapshot: symbolic links are not followed"
    );
}

#[test]
fn a_directory_or_a_missing_parent_is_not_written() {
    let dir = TempDir::new("ai-models-snapshot-dir");
    let error = write_catalog_snapshot(Path::new(&*dir), &built()).expect_err("refused");
    assert_eq!(
        error.0,
        "Could not write catalog snapshot: not a regular file"
    );
    let missing =
        write_catalog_snapshot(&dir.join("no/such/catalog.json"), &built()).expect_err("refused");
    assert!(
        missing
            .0
            .starts_with("Could not write catalog snapshot: \""),
        "{}",
        missing.0
    );
}

#[test]
fn an_invalid_snapshot_is_never_written() {
    let dir = TempDir::new("ai-models-snapshot-invalid");
    let path = dir.join("catalog.json");
    let broken = with(&built(), "model_count", Json::Int(9));
    assert!(write_catalog_snapshot(&path, &broken).is_err());
    assert!(!path.exists());
    assert!(create_catalog_snapshot_file(&broken).is_err());
}

#[test]
fn a_created_snapshot_file_is_unique_private_and_loadable() {
    let first = create_catalog_snapshot_file(&built()).expect("creates");
    let second = create_catalog_snapshot_file(&built()).expect("creates");
    assert_ne!(first, second);
    let name = first.file_name().and_then(|n| n.to_str()).expect("name");
    assert!(name.starts_with("sherpa-model-catalog."), "{name}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&first).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
    assert_eq!(
        load_catalog_snapshot(&first, None, None).expect("loads"),
        built()
    );
    fs::remove_file(&first).expect("cleanup");
    fs::remove_file(&second).expect("cleanup");
}

#[test]
fn loading_reports_a_missing_unparseable_or_oversized_file() {
    let dir = TempDir::new("ai-models-snapshot-load");
    let missing = load_catalog_snapshot(&dir.join("nope.json"), None, None).expect_err("missing");
    assert!(
        missing.0.starts_with("Catalog snapshot not found: "),
        "{}",
        missing.0
    );
    write(&dir, "bad.json", "{\"a\": [1, 2");
    let bad = load_catalog_snapshot(&dir.join("bad.json"), None, None).expect_err("bad");
    assert!(
        bad.0.starts_with("Catalog snapshot is not valid JSON: "),
        "{}",
        bad.0
    );
    write(&dir, "list.json", "[]");
    let list = load_catalog_snapshot(&dir.join("list.json"), None, None).expect_err("list");
    assert_eq!(list.0, "Catalog snapshot must be a JSON object");
    let hostile = dir.join("na\u{1b}[0m\u{202e}me.json");
    let shown = load_catalog_snapshot(&hostile, None, None)
        .expect_err("missing")
        .0;
    assert!(
        !shown.contains('\u{1b}') && !shown.contains('\u{202e}'),
        "{shown}"
    );
}

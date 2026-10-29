//! Offline contracts for GraphRAG and agentic memory (#4886).
use super::{json, run, TempDir};
use crate::docgraph::memory::{chunk_document, create_graph, entity_id, escape_cypher_string, query, gremlin, consolidate};

#[test]
fn chunks_and_identifiers() {
    assert_eq!(chunk_document("abcdef", 4, 2).unwrap(), vec!["abcd", "cdef", "ef"]);
    assert_eq!(chunk_document("é🙂x", 2, 0).unwrap(), vec!["é🙂", "x"]);
    assert!(chunk_document("x", 2, 2).is_err());
    assert_eq!(escape_cypher_string("a'\"\\\n雪"), "a\\'\\\"\\\\\n雪");
    assert_eq!(entity_id("abc"), "ba7816bf8f01cfea414140de5dae2223");
}

#[test]
fn parameterized_requests_and_safe_creation() {
    let request = query("g-abc123def4", "RETURN $x", json(r#"{"x":"' injection"}"#)).unwrap();
    assert_eq!(request.get("parameters"), Some(&json(r#"{"x":"' injection"}"#)));
    assert!(query("g-abc123def4\n", "x", json("{}")).is_err());
    let g = gremlin("g.V().has('name', name)", json(r#"{"name":"x'"}"#));
    assert_eq!(g.get("bindings"), Some(&json(r#"{"name":"x'"}"#)));
    let c = create_graph("example", 32, 1536, "model", true);
    assert_eq!(c.get("deletionProtection"), Some(&json("true")));
    assert_eq!(c.get("publicConnectivity"), Some(&json("false")));
    assert_eq!(c.get("tags"), Some(&json(r#"{"created_by":"neptune-skill","generation_model":"model"}"#)));
}

#[test]
fn consolidation_contains_both_ordered_operations() {
    let calls = consolidate("g-abc123def4").unwrap();
    let first = calls[0].to_compact_string();
    let second = calls[1].to_compact_string();
    assert!(first.contains("7776000000") && first.contains("0.9"));
    assert!(second.contains("0.1") && second.contains("DETACH DELETE"));
}

#[test]
fn command_dry_run_and_replay_validate_recording() {
    let dir = TempDir::new("memory");
    let input = dir.write("input.json", r#"{"graph_id":"g-abc123def4"}"#);
    let dry = run(&["neptune", "memory", "--operation", "consolidate", "--input", &input, "--dry-run"]);
    assert_eq!(dry.status, 0, "{}", dry.stderr);
    assert!(dry.stdout.contains("7776000000"));
    let bad = dir.write("bad.json", r#"{"calls":[{"response":{"results":null}}]}"#);
    let replay = run(&["neptune", "memory", "--operation", "consolidate", "--input", &input, "--recorded", &bad]);
    assert_ne!(replay.status, 0);
}

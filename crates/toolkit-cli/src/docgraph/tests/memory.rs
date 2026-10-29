//! Offline contracts for GraphRAG and agentic memory (#4886).
use super::{json, run, TempDir};
use crate::docgraph::memory::{
    chunk_document, consolidate, create_graph, entity_id, escape_cypher_string, gremlin, query,
};

#[test]
fn chunks_and_identifiers() {
    assert_eq!(
        chunk_document("abcdef", 4, 2).unwrap(),
        vec!["abcd", "cdef", "ef"]
    );
    assert_eq!(chunk_document("é🙂x", 2, 0).unwrap(), vec!["é🙂", "x"]);
    assert!(chunk_document("x", 2, 2).is_err());
    assert_eq!(escape_cypher_string("a'\"\\\n雪"), "a\\'\\\"\\\\\n雪");
    assert_eq!(entity_id("abc"), "ba7816bf8f01cfea414140de5dae2223");
}

#[test]
fn parameterized_requests_and_safe_creation() {
    let request = query("g-abc123def4", "RETURN $x", json(r#"{"x":"' injection"}"#)).unwrap();
    assert_eq!(
        request.get("parameters"),
        Some(&json(r#"{"x":"' injection"}"#))
    );
    assert!(query("g-abc123def4\n", "x", json("{}")).is_err());
    let g = gremlin("g.V().has('name', name)", json(r#"{"name":"x'"}"#));
    assert_eq!(g.get("bindings"), Some(&json(r#"{"name":"x'"}"#)));
    let c = create_graph("example", 32, 1536, "model", true);
    assert_eq!(c.get("deletionProtection"), Some(&json("true")));
    assert_eq!(c.get("publicConnectivity"), Some(&json("false")));
    assert_eq!(
        c.get("tags"),
        Some(&json(
            r#"{"created_by":"neptune-skill","generation_model":"model"}"#
        ))
    );
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
    let dry = run(&[
        "neptune",
        "memory",
        "--operation",
        "consolidate",
        "--input",
        &input,
        "--dry-run",
    ]);
    assert_eq!(dry.status, 0, "{}", dry.stderr);
    assert!(dry.stdout.contains("7776000000"));
    let bad = dir.write("bad.json", r#"{"calls":[{"response":{"results":null}}]}"#);
    let replay = run(&[
        "neptune",
        "memory",
        "--operation",
        "consolidate",
        "--input",
        &input,
        "--recorded",
        &bad,
    ]);
    assert_ne!(replay.status, 0);
}

#[test]
fn retrieve_replays_search_then_three_expansions_per_chunk() {
    use crate::docgraph::memory::{plan, retrieve, RecordedMemory};
    let input = json(r#"{"graph_id":"g-abc123def4","embedding":[0.2],"top_k":2}"#);
    let first = plan("retrieve", &input).unwrap().remove(0);
    let recording = json(
        r#"{"calls":[
      {"response":{"results":[{"id":"c1","text":"first","score":0.9}]}},
      {"response":{"results":[{"name":"Alice"}]}},
      {"response":{"results":[{"name":"Bob"}]}},
      {"response":{"results":[{"id":"c2","text":"neighbor"}]}}
    ]}"#,
    );
    let mut client = RecordedMemory::from_document(&recording).unwrap();
    let result = retrieve(&mut client, "g-abc123def4", &first).unwrap();
    assert_eq!(
        result,
        json(
            r#"[{"chunk_text":"first","score":0.9,"entities":[{"name":"Alice"}],"related_entities":[{"name":"Bob"}],"neighboring_chunks":["neighbor"]}]"#
        )
    );
    assert_eq!(client.unused(), 0);
    let malformed = json(r#"{"calls":[{"response":{"results":null}}]}"#);
    assert!(RecordedMemory::from_document(&malformed).is_err());
    let mismatch = json(r#"{"calls":[{"request":{"unexpected":true},"error":{}}]}"#);
    let mut client = RecordedMemory::from_document(&mismatch).unwrap();
    assert!(retrieve(&mut client, "g-abc123def4", &first).is_err());
}

#[test]
fn memory_requests_preserve_bindings_ttl_and_response_order() {
    use crate::docgraph::memory::{execute, plan, RecordedMemory};
    let input = json(
        r#"{"entity_name":"Alice'","entity_type":"Person","properties":{"x') .drop()":"value"}}"#,
    );
    let request = plan("remember-entity", &input).unwrap().remove(0);
    assert!(!request
        .get("gremlin")
        .unwrap()
        .as_str()
        .unwrap()
        .contains(".drop()"));
    assert_eq!(
        request.get("bindings").unwrap().get("key_0"),
        Some(&json(r#""x') .drop()""#))
    );
    let message = plan(
        "store-message",
        &json(r#"{"now_ms":1000,"session_id":"s","role":"user","content":"hi"}"#),
    )
    .unwrap();
    assert_eq!(
        message[0].get("Item").unwrap().get("ttl"),
        Some(&json("86401"))
    );
    let requests = plan("recent-messages", &json(r#"{"session_id":"s"}"#)).unwrap();
    let mut client = RecordedMemory::from_document(&json(
        r#"{"calls":[{"response":{"Items":[{"timestamp":2},{"timestamp":1}]}}]}"#,
    ))
    .unwrap();
    assert_eq!(
        execute(&mut client, "recent-messages", &requests).unwrap(),
        json(r#"[{"timestamp":1},{"timestamp":2}]"#)
    );
    let mut client =
        RecordedMemory::from_document(&json(r#"{"calls":[{"response":{"id":"g-abc123def4"}}]}"#))
            .unwrap();
    let create = plan("create-memory", &json("{}")).unwrap();
    assert_eq!(
        execute(&mut client, "create-memory", &create).unwrap(),
        json(r#""g-abc123def4""#)
    );
}

#[test]
fn graph_ingestion_and_hybrid_plans_preserve_sequence() {
    use crate::docgraph::memory::plan;
    let chunk = json(
        r#"{"graph_id":"g-abc123def4","chunk_id":"c","doc_id":"d","text":"x","sequence":1,"embedding":[0.1],"prev_chunk_id":"p"}"#,
    );
    let steps = plan("ingest-chunk", &chunk).unwrap();
    assert_eq!(steps.len(), 4);
    assert!(steps[1]
        .get("queryString")
        .unwrap()
        .as_str()
        .unwrap()
        .contains("vectors.upsert"));
    assert!(steps[3]
        .get("queryString")
        .unwrap()
        .as_str()
        .unwrap()
        .contains("NEXT"));
    let input = json(r#"{"graph_id":"g-abc123def4","entity_name":"A'","embedding":[0.1]}"#);
    let hybrid = plan("hybrid-recall", &input).unwrap();
    assert_eq!(hybrid.len(), 2);
    assert!(hybrid[1].to_compact_string().contains("notEquals"));
}

//! Offline request builders for Neptune GraphRAG and agentic memory.
use std::fmt::Write;
use toolkit_core::{sha256_hex, Json};

pub fn chunk_document(
    text: &str,
    size: usize,
    overlap: usize,
) -> Result<Vec<String>, &'static str> {
    if size == 0 || overlap >= size {
        return Err("chunk size must be positive and exceed overlap");
    }
    let chars: Vec<char> = text.chars().collect();
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < chars.len() {
        let end = start.saturating_add(size).min(chars.len());
        chunks.push(chars[start..end].iter().collect());
        start = start.saturating_add(size - overlap);
    }
    Ok(chunks)
}
#[must_use]
pub fn escape_cypher_string(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\'', "\\'")
        .replace('"', "\\\"")
}
#[must_use]
pub fn entity_id(name: &str) -> String {
    sha256_hex(name.as_bytes())[..32].to_string()
}
pub fn query(id: &str, statement: &str, parameters: Json) -> Result<Json, &'static str> {
    // recorded-shape: caller-provided query parameters must be an object, not a response.
    if !matches!(parameters, Json::Object(_)) {
        return Err("parameters must be an object");
    }
    let Some(suffix) = id.strip_prefix("g-") else {
        return Err("invalid graph_id");
    };
    if suffix.len() < 10
        || !suffix
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
    {
        return Err("invalid graph_id");
    }
    let mut pairs = vec![
        ("graphIdentifier", Json::string(id)),
        ("queryString", Json::string(statement)),
        ("language", Json::string("OPEN_CYPHER")),
    ];
    // recorded-shape: this is the already validated caller parameter object.
    if !matches!(&parameters, Json::Object(p) if p.is_empty()) {
        pairs.push(("parameters", parameters));
    }
    Ok(Json::object(pairs))
}
#[must_use]
pub fn gremlin(statement: &str, bindings: Json) -> Json {
    Json::object(vec![
        ("gremlin", Json::string(statement)),
        ("bindings", bindings),
    ])
}
#[must_use]
pub fn create_graph(name: &str, memory: i64, dimension: i64, model: &str, rag: bool) -> Json {
    let mut p = vec![
        ("graphName", Json::string(name)),
        ("provisionedMemory", Json::Int(memory)),
        ("publicConnectivity", Json::Bool(false)),
        (
            "vectorSearchConfiguration",
            Json::object(vec![("dimension", Json::Int(dimension))]),
        ),
        ("deletionProtection", Json::Bool(true)),
        (
            "tags",
            Json::object(vec![
                ("created_by", Json::string("neptune-skill")),
                ("generation_model", Json::string(model)),
            ]),
        ),
    ];
    if rag {
        p.push(("replicaCount", Json::Int(0)));
    }
    Json::object(p)
}
pub fn consolidate(id: &str) -> Result<Vec<Json>, &'static str> {
    Ok(vec![query(id, "MATCH (f:Fact) WHERE f.updated_at < timestamp() - 7776000000 SET f.confidence = f.confidence * 0.9", Json::object(vec![]))?, query(id, "MATCH (f:Fact) WHERE f.confidence < 0.1 DETACH DELETE f", Json::object(vec![]))?])
}

fn arg<'a>(input: &'a Json, key: &str) -> Result<&'a Json, String> {
    input.get(key).ok_or_else(|| format!("missing {key}"))
}
fn string<'a>(input: &'a Json, key: &str) -> Result<&'a str, String> {
    arg(input, key)?
        .as_str()
        .ok_or_else(|| format!("{key} must be a string"))
}
fn params(input: &Json, keys: &[(&str, &str)]) -> Result<Json, String> {
    let mut values = Vec::new();
    for (target, source) in keys {
        values.push((*target, arg(input, source)?.clone()));
    }
    Ok(Json::object(values))
}
fn cypher(id: &str, statement: &str, p: Json) -> Result<Json, String> {
    query(id, statement, p).map_err(str::to_string)
}
fn filter(label: &str) -> Json {
    Json::string(
        Json::object(vec![(
            "equals",
            Json::object(vec![
                ("property", Json::string("~label")),
                ("value", Json::string(label)),
            ]),
        )])
        .to_compact_string(),
    )
}
fn vector(id: &str, input: &Json, label: &str, top: i64, entity: bool) -> Result<Json, String> {
    let p = Json::object(vec![
        ("embedding", arg(input, "embedding")?.clone()),
        (
            "top_k",
            input.get("top_k").cloned().unwrap_or(Json::Int(top)),
        ),
        ("vertex_filter", filter(label)),
    ]);
    let ret = if entity {
        "node.name AS name, node.type AS type, node.description AS description, score"
    } else {
        "node.id AS id, node.text AS text, score"
    };
    cypher(id, &format!("CALL neptune.algo.vectors.topK.byEmbedding({{embedding: $embedding, topK: $top_k, vertexFilter: $vertex_filter}}) YIELD node, score RETURN {ret} ORDER BY score DESC"), p)
}
/// Build a deterministic sequence of requests from operation arguments.
pub fn plan(op: &str, input: &Json) -> Result<Vec<Json>, String> {
    let empty = || Json::object(vec![]);
    if op == "create-graphrag" || op == "create-memory" {
        let rag = op == "create-graphrag";
        let name = if rag {
            string(input, "graph_name")?
        } else {
            input
                .get("graph_name")
                .and_then(Json::as_str)
                .unwrap_or("agent-memory")
        };
        let number = |key: &str, default| match input.get(key) {
            Some(Json::Int(n)) => Ok(*n),
            None => Ok(default),
            Some(_) => Err(format!("{key} must be an integer")),
        };
        return Ok(vec![create_graph(
            name,
            number("memory_gb", if rag { 32 } else { 16 })?,
            number("embedding_dimension", 1536)?,
            input
                .get("generation_model")
                .and_then(Json::as_str)
                .unwrap_or("unknown"),
            rag,
        )]);
    }
    if op == "remember-relationship" {
        let mut bindings = params(
            input,
            &[
                ("name_a", "entity_a"),
                ("name_b", "entity_b"),
                ("rel_type", "rel_type"),
            ],
        )?;
        // recorded-shape: bindings are constructed from caller input, not a response.
        if let Json::Object(ref mut fields) = bindings {
            fields.push((
                "confidence".to_string(),
                input.get("confidence").cloned().unwrap_or(Json::Float(1.0)),
            ));
        }
        return Ok(vec![gremlin("g.V().has('Entity','name',name_a).as('a').V().has('Entity','name',name_b).as('b').coalesce(select('a').outE('RELATED_TO').where(inV().as('b')),select('a').addE('RELATED_TO').to(select('b'))).property('type',rel_type).property('confidence',confidence).property('updated_at',new Date().getTime())", bindings)]);
    }
    if op == "recall-entity" {
        return Ok(vec![gremlin("g.V().has('Entity','name',entity_name).project('entity','relationships','conversations').by(valueMap()).by(bothE('RELATED_TO').project('type','target','confidence').by(values('type')).by(otherV().values('name')).by(values('confidence')).fold()).by(in('ABOUT').hasLabel('Conversation').order().by('date',desc).limit(5).valueMap('summary','date').fold())",params(input,&[("entity_name","entity_name")])?)]);
    }
    if op == "remember-entity" {
        let mut bindings = vec![
            (
                "entity_name".to_string(),
                arg(input, "entity_name")?.clone(),
            ),
            (
                "entity_type".to_string(),
                arg(input, "entity_type")?.clone(),
            ),
        ];
        let mut statement = "g.V().has('Entity','name',entity_name).fold().coalesce(unfold(),addV('Entity').property('name',entity_name)).property('type',entity_type).property('updated_at',new Date().getTime())".to_string();
        if let Some(properties) = input.get("properties") {
            // recorded-shape: caller-supplied properties must be an object.
            let Json::Object(properties) = properties else {
                return Err("properties must be an object".to_string());
            };
            for (index, (key, value)) in properties.iter().enumerate() {
                let k = format!("key_{index}");
                let v = format!("value_{index}");
                let _ = write!(statement, ".property({k},{v})");
                bindings.push((k, Json::string(key)));
                bindings.push((v, value.clone()));
            }
        }
        return Ok(vec![gremlin(&statement, Json::Object(bindings))]);
    }
    if op == "store-message" {
        let Json::Int(now) = arg(input, "now_ms")? else {
            return Err("now_ms must be an integer".to_string());
        };
        let ttl = now
            .div_euclid(1000)
            .checked_add(86400)
            .ok_or("timestamp overflow")?;
        return Ok(vec![Json::object(vec![
            ("operation", Json::string("put_item")),
            (
                "table",
                input
                    .get("table")
                    .cloned()
                    .unwrap_or(Json::string("agent-short-term-memory")),
            ),
            (
                "Item",
                Json::object(vec![
                    ("session_id", arg(input, "session_id")?.clone()),
                    ("timestamp", Json::Int(*now)),
                    ("role", arg(input, "role")?.clone()),
                    ("content", arg(input, "content")?.clone()),
                    ("ttl", Json::Int(ttl)),
                ]),
            ),
        ])]);
    }
    if op == "recent-messages" {
        return Ok(vec![Json::object(vec![
            ("operation", Json::string("query")),
            (
                "table",
                input
                    .get("table")
                    .cloned()
                    .unwrap_or(Json::string("agent-short-term-memory")),
            ),
            ("KeyConditionExpression", Json::string("session_id = :sid")),
            (
                "ExpressionAttributeValues",
                Json::object(vec![(":sid", arg(input, "session_id")?.clone())]),
            ),
            ("ScanIndexForward", Json::Bool(false)),
            (
                "Limit",
                input.get("limit").cloned().unwrap_or(Json::Int(20)),
            ),
        ])]);
    }
    let id = string(input, "graph_id")?;
    let q = |s, p| cypher(id, s, p);
    match op {
        "query" => Ok(vec![q(string(input,"query")?, input.get("parameters").cloned().unwrap_or_else(empty))?]),
        "consolidate" => consolidate(id).map_err(str::to_string),
        "vector-search" => Ok(vec![vector(id,input,input.get("vertex_label").and_then(Json::as_str).unwrap_or("Chunk"),5,false)?]),
        "retrieve" => Ok(vec![vector(id,input,"Chunk",5,false)?]),
        "semantic-recall" => Ok(vec![vector(id,input,"Entity",10,true)?]),
        "hybrid-recall" => {
            let name = arg(input,"entity_name")?.clone();
            let filter = Json::object(vec![("andAll",Json::Array(vec![Json::object(vec![("equals",Json::object(vec![("property",Json::string("~label")),("value",Json::string("Entity"))]))]),Json::object(vec![("notEquals",Json::object(vec![("property",Json::string("name")),("value",name.clone())]))])]))]).to_compact_string();
            Ok(vec![q("MATCH (e:Entity {name: $name})-[r:RELATED_TO]-(related) RETURN related.name AS name, related.type AS type, r.type AS relationship, r.confidence AS confidence ORDER BY r.confidence DESC LIMIT 20",Json::object(vec![("name",name)]))?,q("CALL neptune.algo.vectors.topK.byEmbedding({embedding: $embedding, topK: 10, vertexFilter: $entity_filter}) YIELD node, score RETURN node.name AS name, node.type AS type, score",Json::object(vec![("embedding",arg(input,"embedding")?.clone()),("entity_filter",Json::string(filter))]))?])
        },
        "ingest-document" => Ok(vec![q("CREATE (d:Document {id: $doc_id, title: $title, source: $source})",params(input,&[("doc_id","doc_id"),("title","title"),("source","source")])?)?]),
        "store-embedding" => Ok(vec![q("MATCH (n {id: $vertex_id}) CALL neptune.algo.vectors.upsert(n, $embedding) YIELD node RETURN node.id",params(input,&[("vertex_id","vertex_id"),("embedding","embedding")])?)?]),
        "link-chunk-entity" => Ok(vec![q("MATCH (c:Chunk {id: $chunk_id}), (e:Entity {name: $name}) CREATE (c)-[:MENTIONS]->(e)",params(input,&[("chunk_id","chunk_id"),("name","entity_name")])?)?]),
        "link-entities" => Ok(vec![q("MATCH (s:Entity {name: $source}), (t:Entity {name: $target}) MERGE (s)-[:RELATED_TO {type: $rel_type}]->(t)",params(input,&[("source","source"),("target","target"),("rel_type","rel_type")])?)?]),
        "remember-embedding" => Ok(vec![q("MERGE (e:Entity {name: $name}) SET e.description = $description, e.updated_at = timestamp()",params(input,&[("name","entity_name"),("description","description")])?)?,q("MATCH (e:Entity {name: $name}) CALL neptune.algo.vectors.upsert(e, $embedding) YIELD node RETURN node.name",params(input,&[("name","entity_name"),("embedding","embedding")])?)?]),
        "ingest-entity" => {
            let name = string(input,"name")?; let eid = entity_id(name);
            Ok(vec![q("MERGE (e:Entity {name: $name}) ON CREATE SET e.type = $type, e.description = $desc, e.id = $entity_id",Json::object(vec![("name",Json::string(name)),("type",arg(input,"entity_type")?.clone()),("desc",arg(input,"description")?.clone()),("entity_id",Json::string(&eid))]))?,q("MATCH (n {id: $vertex_id}) CALL neptune.algo.vectors.upsert(n, $embedding) YIELD node RETURN node.id",Json::object(vec![("vertex_id",Json::string(eid)),("embedding",arg(input,"embedding")?.clone())]))?])
        },
        "ingest-chunk" => {
            let mut calls = vec![q("CREATE (c:Chunk {id: $chunk_id, text: $text, sequence: $seq})",params(input,&[("chunk_id","chunk_id"),("text","text"),("seq","sequence")])?)?,q("MATCH (n {id: $vertex_id}) CALL neptune.algo.vectors.upsert(n, $embedding) YIELD node RETURN node.id",params(input,&[("vertex_id","chunk_id"),("embedding","embedding")])?)?,q("MATCH (d:Document {id: $doc_id}), (c:Chunk {id: $chunk_id}) CREATE (d)-[:HAS_CHUNK]->(c)",params(input,&[("doc_id","doc_id"),("chunk_id","chunk_id")])?)?];
            if input.get("prev_chunk_id").and_then(Json::as_str).is_some_and(|s| !s.is_empty()) { calls.push(q("MATCH (prev:Chunk {id: $prev_id}), (curr:Chunk {id: $curr_id}) CREATE (prev)-[:NEXT]->(curr)",params(input,&[("prev_id","prev_chunk_id"),("curr_id","chunk_id")])?)?); }
            Ok(calls)
        },
        _ => Err("unknown operation".to_string()),
    }
}

pub fn retrieve(session: &mut dyn MemoryClient, id: &str, first: &Json) -> Result<Json, String> {
    let response = session.submit(first)?;
    let chunks = toolkit_aws::recorded::optional_list(&response, "results")
        .map_err(|e| e.to_string())?
        .to_vec();
    let mut expanded = Vec::new();
    for chunk in chunks {
        let cid = Json::string(
            toolkit_aws::recorded::optional_text(&chunk, "id")
                .map_err(|e| e.to_string())?
                .ok_or("chunk missing id")?,
        );
        let p = || Json::object(vec![("cid", cid.clone())]);
        let mut call = |s| -> Result<Vec<Json>, String> {
            let request = cypher(id, s, p())?;
            let response = session.submit(&request)?;
            Ok(toolkit_aws::recorded::optional_list(&response, "results")
                .map_err(|e| e.to_string())?
                .to_vec())
        };
        let entities = call("MATCH (c:Chunk {id: $cid})-[:MENTIONS]->(e:Entity) RETURN e.name AS name, e.type AS type, e.description AS description")?;
        let related = call("MATCH (c:Chunk {id: $cid})-[:MENTIONS]->(e:Entity)-[:RELATED_TO]-(r:Entity) RETURN DISTINCT r.name AS name, r.type AS type, r.description AS description")?;
        let neighbors =
            call("MATCH (c:Chunk {id: $cid})-[:NEXT]-(n:Chunk) RETURN n.text AS text, n.id AS id")?;
        let texts = neighbors
            .iter()
            .map(|n| {
                toolkit_aws::recorded::optional_text(n, "text")
                    .map_err(|e| e.to_string())?
                    .map(Json::string)
                    .ok_or_else(|| "neighbor missing text".to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        let text = toolkit_aws::recorded::optional_text(&chunk, "text")
            .map_err(|e| e.to_string())?
            .ok_or("chunk missing text")?;
        let score =
            toolkit_aws::recorded::optional_member(&chunk, "score", "a number", |v| match v {
                Json::Int(_) | Json::Float(_) => Some(v.clone()),
                _ => None,
            })
            .map_err(|e| e.to_string())?
            .ok_or("chunk missing score")?;
        expanded.push(Json::object(vec![
            ("chunk_text", Json::string(text)),
            ("score", score),
            ("entities", Json::Array(entities)),
            ("related_entities", Json::Array(related)),
            ("neighboring_chunks", Json::Array(texts)),
        ]));
    }
    Ok(Json::Array(expanded))
}

/// An injected offline or live-compatible request client.
pub trait MemoryClient {
    /// Execute one request and return its response object.
    fn submit(&mut self, request: &Json) -> Result<Json, String>;
}

/// Ordered, request-matching recorded responses.
pub struct RecordedMemory {
    queue: toolkit_aws::recorded::StepQueue<()>,
}
impl RecordedMemory {
    /// Parse and validate all recorded steps before replay.
    pub fn from_document(document: &Json) -> Result<Self, String> {
        use toolkit_aws::recorded::{
            optional_list, optional_text, step_fields, KeyOrder, RecordedOutcome, Step, StepQueue,
        };
        let calls = document
            .get("calls")
            .and_then(super::snapshot::list)
            .ok_or("recording needs a calls array")?;
        let mut queue = StepQueue::new(KeyOrder::Ignored);
        for (index, call) in calls.iter().enumerate() {
            let fields =
                step_fields(call).map_err(|_| format!("recorded call {index}: invalid step"))?;
            let request = Some(
                fields
                    .request
                    .cloned()
                    .ok_or_else(|| format!("recorded call {index}: missing request"))?,
            );
            let step = match fields.outcome {
                RecordedOutcome::Response(response) => {
                    for key in ["results", "Items"] {
                        optional_list(response, key)
                            .map_err(|e| format!("recorded call {index}: {e}"))?;
                    }
                    optional_text(response, "id")
                        .map_err(|e| format!("recorded call {index}: {e}"))?;
                    Step::response(request, response.clone())
                }
                RecordedOutcome::Error(_) => Step::failure(
                    request,
                    toolkit_aws::AwsError::connect("recorded service error"),
                ),
            };
            queue.push((), step);
        }
        Ok(Self { queue })
    }
    /// Number of unmatched entries.
    #[must_use]
    pub fn unused(&self) -> usize {
        self.queue.unconsumed()
    }
}
impl MemoryClient for RecordedMemory {
    fn submit(&mut self, request: &Json) -> Result<Json, String> {
        self.queue
            .replay(&(), request)
            .map_err(|_| "recorded request mismatch or exhausted".to_string())?
            .map_err(|_| "recorded service error".to_string())
    }
}
/// Execute a planned operation and shape the script's response.
pub fn execute(client: &mut dyn MemoryClient, op: &str, requests: &[Json]) -> Result<Json, String> {
    let mut responses = Vec::new();
    for request in requests {
        responses.push(client.submit(request)?);
    }
    if op == "create-graphrag" || op == "create-memory" {
        let id = toolkit_aws::recorded::optional_text(&responses[0], "id")
            .map_err(|e| e.to_string())?
            .ok_or("response missing id")?;
        return Ok(Json::string(id));
    }
    if op == "recent-messages" {
        let mut items = toolkit_aws::recorded::optional_list(&responses[0], "Items")
            .map_err(|e| e.to_string())?
            .to_vec();
        items.reverse();
        return Ok(Json::Array(items));
    }
    if op == "hybrid-recall" {
        let list = |i| -> Result<Json, String> {
            Ok(Json::Array(
                toolkit_aws::recorded::optional_list(&responses[i], "results")
                    .map_err(|e| e.to_string())?
                    .to_vec(),
            ))
        };
        return Ok(Json::object(vec![
            ("graph_recall", list(0)?),
            ("semantic_recall", list(1)?),
        ]));
    }
    let results = responses
        .iter()
        .map(|response| {
            toolkit_aws::recorded::optional_list(response, "results")
                .map(|v| Json::Array(v.to_vec()))
                .map_err(|e| e.to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Json::Array(results))
}

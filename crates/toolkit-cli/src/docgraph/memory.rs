//! Offline request builders for Neptune GraphRAG and agentic memory.
use toolkit_core::{sha256_hex, Json};

pub(super) fn chunk_document(
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
        start += size - overlap;
    }
    Ok(chunks)
}
pub(super) fn escape_cypher_string(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\'', "\\'")
        .replace('"', "\\\"")
}
pub(super) fn entity_id(name: &str) -> String {
    sha256_hex(name.as_bytes())[..32].to_string()
}
pub(super) fn query(id: &str, statement: &str, parameters: Json) -> Result<Json, &'static str> {
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
    if !matches!(&parameters, Json::Object(p) if p.is_empty()) {
        pairs.push(("parameters", parameters));
    }
    Ok(Json::object(pairs))
}
pub(super) fn gremlin(statement: &str, bindings: Json) -> Json {
    Json::object(vec![
        ("gremlin", Json::string(statement)),
        ("bindings", bindings),
    ])
}
pub(super) fn create_graph(
    name: &str,
    memory: i64,
    dimension: i64,
    model: &str,
    rag: bool,
) -> Json {
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
pub(super) fn consolidate(id: &str) -> Result<Vec<Json>, &'static str> {
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
fn vector(id: &str, input: &Json, label: &str, top: i64) -> Result<Json, String> {
    let p = Json::object(vec![
        ("embedding", arg(input, "embedding")?.clone()),
        (
            "top_k",
            input.get("top_k").cloned().unwrap_or(Json::Int(top)),
        ),
        ("vertex_filter", filter(label)),
    ]);
    let ret = if label == "Chunk" {
        "node.id AS id, node.text AS text, score"
    } else {
        "node.name AS name, node.type AS type, node.description AS description, score"
    };
    cypher(id, &format!("CALL neptune.algo.vectors.topK.byEmbedding({{embedding: $embedding, topK: $top_k, vertexFilter: $vertex_filter}}) YIELD node, score RETURN {ret} ORDER BY score DESC"), p)
}
/// Build a deterministic sequence of requests from operation arguments.
pub(super) fn plan(op: &str, input: &Json) -> Result<Vec<Json>, String> {
    let empty = || Json::object(vec![]);
    if op == "create-graphrag" || op == "create-memory" {
        let rag = op == "create-graphrag";
        let name = input
            .get("graph_name")
            .and_then(Json::as_str)
            .unwrap_or(if rag { "graphrag" } else { "agent-memory" });
        let number = |key: &str, default| match input.get(key) {
            Some(Json::Int(n)) => *n,
            _ => default,
        };
        return Ok(vec![create_graph(
            name,
            number("memory_gb", if rag { 32 } else { 16 }),
            number("embedding_dimension", 1536),
            input
                .get("generation_model")
                .and_then(Json::as_str)
                .unwrap_or("unknown"),
            rag,
        )]);
    }
    if op == "remember-relationship" {
        return Ok(vec![gremlin("g.V().has('Entity','name',name_a).as('a').V().has('Entity','name',name_b).as('b').coalesce(select('a').outE('RELATED_TO').where(inV().as('b')),select('a').addE('RELATED_TO').to(select('b'))).property('type',rel_type).property('confidence',confidence).property('updated_at',new Date().getTime())", params(input, &[("name_a","entity_a"),("name_b","entity_b"),("rel_type","rel_type"),("confidence","confidence")])?)]);
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
            let Json::Object(properties) = properties else {
                return Err("properties must be an object".to_string());
            };
            for (index, (key, value)) in properties.iter().enumerate() {
                let k = format!("key_{index}");
                let v = format!("value_{index}");
                statement.push_str(&format!(".property({k},{v})"));
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
        "vector-search" | "retrieve" => Ok(vec![vector(id,input,"Chunk",5)?]),
        "semantic-recall" => Ok(vec![vector(id,input,"Entity",10)?]),
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

pub(super) fn retrieve(
    session: &mut dyn super::seed::GremlinSession,
    id: &str,
    first: &Json,
) -> Result<Json, String> {
    let chunks = session
        .submit(first)
        .map_err(|_| "recorded query failed".to_string())?;
    let mut expanded = Vec::new();
    for chunk in chunks {
        let cid = arg(&chunk, "id")?.clone();
        let p = || Json::object(vec![("cid", cid.clone())]);
        let mut call = |s| -> Result<Vec<Json>, String> {
            let request = cypher(id, s, p())?;
            session
                .submit(&request)
                .map_err(|_| "recorded query failed".to_string())
        };
        let entities = call("MATCH (c:Chunk {id: $cid})-[:MENTIONS]->(e:Entity) RETURN e.name AS name, e.type AS type, e.description AS description")?;
        let related = call("MATCH (c:Chunk {id: $cid})-[:MENTIONS]->(e:Entity)-[:RELATED_TO]-(r:Entity) RETURN DISTINCT r.name AS name, r.type AS type, r.description AS description")?;
        let neighbors =
            call("MATCH (c:Chunk {id: $cid})-[:NEXT]-(n:Chunk) RETURN n.text AS text, n.id AS id")?;
        let texts = neighbors
            .iter()
            .map(|n| arg(n, "text").cloned())
            .collect::<Result<Vec<_>, _>>()?;
        expanded.push(Json::object(vec![
            ("chunk_text", arg(&chunk, "text")?.clone()),
            ("score", arg(&chunk, "score")?.clone()),
            ("entities", Json::Array(entities)),
            ("related_entities", Json::Array(related)),
            ("neighboring_chunks", Json::Array(texts)),
        ]));
    }
    Ok(Json::Array(expanded))
}

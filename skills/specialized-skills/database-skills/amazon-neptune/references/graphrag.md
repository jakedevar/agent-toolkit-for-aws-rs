# GraphRAG with Neptune Analytics

## Overview

GraphRAG uses a knowledge graph to improve RAG accuracy over vector-only retrieval.
Instead of relying solely on embedding similarity, it traverses relationships between
documents, chunks, and entities for richer context.

**Two paths to GraphRAG with Neptune:**

### Path 1: Managed (Bedrock Knowledge Bases GraphRAG) — recommended for most users

Amazon Bedrock Knowledge Bases offers fully managed GraphRAG. No graph expertise required.

1. In the Bedrock console, create or edit a Knowledge Base
2. Select your unstructured data sources from S3
3. Enable GraphRAG — Bedrock automatically builds a graph from your data
4. Graph is stored in Neptune Analytics (default graph store)
5. Query using existing Bedrock KB APIs (`retrieveAndGenerate`)

Bedrock KB GraphRAG automatically extracts entities and relationships from your documents,
builds the graph, and combines graph traversal with vector search during retrieval. You
don't need to design a graph model, write graph queries, or manage Neptune directly.

**When to use the managed path:**

- You have unstructured documents (PDFs, text files) in S3
- You want GraphRAG without learning graph query languages
- You want a fully managed experience with no infrastructure to manage
- You're already using or evaluating Bedrock Knowledge Bases

**When to use the custom path (below):**

- You need a custom graph model (not auto-generated from documents)
- You have structured data that should be modeled as specific entity types
- You need control over the entity extraction and graph construction process
- You want to combine your own existing graph with RAG retrieval

### Path 2: Custom pipeline (Neptune Analytics) — for full control

Neptune Analytics is recommended for custom GraphRAG because it stores both graph structure
and embeddings in one service — no separate vector store needed.

**When to use GraphRAG over traditional RAG:**

- Multiple documents with cross-references or shared entities
- Questions about relationships ("how are X and Y related?")
- Entity-centric retrieval across a corpus
- Community-based summarization

**When traditional RAG is sufficient:**

- Single document Q&A
- Simple semantic search over passages

## Architecture

```
Documents → Chunk → Extract Entities (LLM) → Build Graph → Store Embeddings
                                                                    ↓
Query → Vector Search (similar chunks) → Graph Expansion (entities, neighbors) → LLM
```

## Graph Model

```
(Document) -[HAS_CHUNK]→ (Chunk {text, embedding})
(Chunk) -[MENTIONS]→ (Entity {name, type, embedding})
(Chunk) -[NEXT]→ (Chunk)
(Entity) -[RELATED_TO]→ (Entity)
```

## Implementation

The Rust toolkit builds requests offline and can replay recorded responses. It
never contacts Neptune in this build. Supply the operation arguments as JSON:

```bash
cat > request.json <<'JSON'
{"graph_id":"g-abc123def4","embedding":[0.1,0.2],"top_k":5}
JSON
toolkit neptune memory --operation retrieve --input request.json --dry-run
toolkit neptune memory --operation retrieve --input request.json --recorded calls.json
```

The dry run prints the initial vector request as a JSON array. Replay follows
it with three parameterized queries per returned chunk for entities, related
entities and neighboring chunks. Use `create-graphrag`, `ingest-document`,
`ingest-chunk`, `ingest-entity`, `store-embedding`, `vector-search`,
`link-chunk-entity`, `link-entities` or `query` for other request plans.
`create-graphrag` takes `graph_name` and optional `memory_gb`,
`embedding_dimension`, and `generation_model`; its request includes deletion
protection, private connectivity, and the mandatory tags. See
`docs/migration/docgraph.md` for inputs and recording format. Always bind
query values in `parameters` rather than interpolating them into query text.

### Embedding model note

Code uses `embedding_model.encode(text)` as placeholder. Replace with:

- Amazon Bedrock Titan Embeddings (`bedrock.invoke_model(...)`)
- sentence-transformers (`.encode()`)
- OpenAI (`openai.embeddings.create(...)`)

## Persistence

Neptune Analytics is **ephemeral**. For production:

- Keep graph running (pay NCU hours), OR
- Export to S3 periodically and reload, OR
- Use Neptune Database as persistent store, sync to Analytics for retrieval

## Common Mistakes

1. **Skipping entity resolution** — duplicates create disconnected subgraphs
2. **Chunks too large** — keep 256–512 tokens with overlap
3. **Not storing entity embeddings** — limits semantic search to chunks only
4. **Using Neptune Database for GraphRAG** — Analytics has built-in vectors
5. **String interpolation** — use parameterized queries to prevent injection
6. **Filtering labels after vector `topK`** — use `vertexFilter` so unrelated
   labels do not consume the chunk result slots
7. **Ignoring community detection** — enables multi-level summarization

## Additional Resources

- AWS docs: "Neptune Analytics vector search", "Neptune Analytics openCypher"
- Offline Rust requests: `toolkit neptune memory` (see `docs/migration/docgraph.md`)
- Related sub-skills: `analytics-vs-database` (setup), `connectivity` (connection)

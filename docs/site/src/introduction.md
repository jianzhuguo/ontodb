# OntoDB

**An ontology-driven, semantic multi-modal database.**

OntoDB uniquely combines **SQL**, **SPARQL**, **vector search**, and **property graph** queries in a single database engine — with built-in **OWL ontology reasoning**.

## Why OntoDB?

| Feature | OntoDB | PostgreSQL+pgvector | Neo4j | Pinecone | SurrealDB |
|---------|--------|-------------------|-------|----------|-----------|
| SQL queries | ✅ | ✅ | ❌ | ❌ | ✅ |
| Vector search | ✅ native | ✅ extension | ❌ | ✅ | ❌ |
| Property graph | ✅ | ❌ | ✅ | ❌ | ✅ |
| SPARQL | ✅ | ❌ | ❌ | ❌ | ❌ |
| Ontology reasoning | ✅ unique | ❌ | ❌ | ❌ | ❌ |
| Single engine | ✅ | ❌ | ❌ | ❌ | ✅ |

**OntoDB is the only database that embeds OWL reasoning directly in the query engine.** No other database lets you query across SQL, vectors, graphs, and ontologies without stitching multiple systems together.

## Key capabilities

- **SQL** — Full DDL/DML with JOINs, subqueries, CTEs, window functions
- **Vector search** — HNSW index with 100% recall, hybrid SQL+vector queries
- **Property graph** — CRUD, BFS/DFS traversal, shortest path
- **SPARQL** — W3C-compliant query over RDF data
- **Ontology reasoning** — Subclass propagation, property inference, OWL restrictions
- **LSM-Tree storage** — Write-optimized, MVCC transactions, background compaction
- **HTTP API** — RESTful with API key auth, rate limiting, Prometheus metrics

## Quick links

- [Quick Start](./quickstart.md) — Get running in 5 minutes
- [API Reference](./api.md) — Full HTTP API docs
- [Python SDK](./sdks/python.md) | [TypeScript SDK](./sdks/typescript.md)
- [Examples](./examples.md) — RAG, knowledge graph, multi-modal demos
- [Deployment](./deployment.md) — Docker, Kubernetes, production guide

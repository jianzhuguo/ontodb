# FAQ

## General

### What is OntoDB?

OntoDB is an ontology-driven, semantic multi-modal database. It uniquely combines SQL, SPARQL, vector search, and property graph queries in a single engine with built-in OWL reasoning.

### How is OntoDB different from PostgreSQL?

PostgreSQL is a relational database that can be extended with pgvector for vector search. OntoDB is a multi-modal database that natively supports SQL, vectors, graphs, and SPARQL in a single engine — no extensions needed. The key differentiator is OntoDB's built-in ontology reasoning.

### How is OntoDB different from Neo4j?

Neo4j is a dedicated graph database. OntoDB includes a property graph model alongside SQL, vector search, and SPARQL. You don't need a separate graph database — OntoDB handles it all.

### Is OntoDB production-ready?

OntoDB is currently in beta (v0.6.2). It's suitable for development and testing. Production use requires the commercial version with clustering, enterprise security, and support.

## Technical

### What storage engine does OntoDB use?

OntoDB uses an LSM-Tree (Log-Structured Merge Tree) storage engine with WAL, SSTable, MemTable, and leveled compaction. It supports MVCC transactions with snapshot isolation.

### What vector index does OntoDB use?

OntoDB uses HNSW (Hierarchical Navigable Small World) for vector indexing, achieving 100% recall rate.

### Does OntoDB support ACID transactions?

Yes. OntoDB supports multi-statement transactions with `BEGIN`/`COMMIT`/`ROLLBACK` and snapshot isolation.

### What's the maximum dataset size?

OntoDB is designed for datasets up to ~100GB on a single node. For larger datasets, the commercial version offers sharding and clustering.

### Can I use OntoDB with my existing ORM?

Not directly — OntoDB has its own SQL dialect. However, the HTTP API is RESTful and easy to integrate with any language. SDKs are available for Python and TypeScript.

## Deployment

### How do I run OntoDB in Docker?

```bash
docker run -d -p 7912:7912 -p 7913:7913 -v ontodb_data:/data ontodb/ontodb:latest
```

### How do I enable authentication?

```bash
ontodb-server --http 127.0.0.1:7912 --auth --api-keys-file config/api_keys.json
```

### How do I back up my data?

```bash
curl -X POST http://localhost:7912/api/backup \
  -H "Content-Type: application/json" \
  -d '{"path": "/backups/2026-08-08"}'
```

### How do I monitor OntoDB?

OntoDB exposes Prometheus metrics at `/metrics` and JSON metrics at `/api/metrics`. Grafana dashboards are included in `deploy/monitoring/`.

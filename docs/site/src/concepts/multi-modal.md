# Multi-Modal Model

OntoDB unifies six data modalities in a single storage engine.

## The Six Modalities

| Modality | Use Case | OntoDB Feature |
|----------|----------|----------------|
| **Relational** | Structured data, CRUD | SQL DDL/DML, JOINs, CTEs |
| **Graph** | Relationships, traversal | Property graph, BFS/DFS, shortest path |
| **Vector** | Semantic similarity | HNSW index, L2/Cosine/InnerProduct |
| **Time-Series** | Metrics, logs | TSM compression, time-based queries |
| **Spatial** | Location, GIS | R*-tree, Geohash, spatial predicates |
| **Ontology** | Semantic reasoning | OWL 2 RL rules, class/property inference |

## Why Multi-Modal?

Traditional architectures require stitching multiple databases together:

```
Traditional:
  PostgreSQL (relational) + Neo4j (graph) + Pinecone (vector) + InfluxDB (time-series)
  = 4 systems to manage, 4 query languages, 4 data pipelines

OntoDB:
  One engine, one query language (OntoQL), one data pipeline
```

## Unified Query Language (OntoQL)

OntoQL extends SQL with semantic capabilities:

```sql
-- Relational: standard SQL
SELECT name, price FROM Product WHERE price > 10;

-- Vector: semantic similarity
VECTOR SEARCH ON Product (embedding) QUERY [0.1, 0.2, 0.3] TOP 10;

-- Graph: relationship traversal
GRAPH TRAVERSE FROM 'Product::1' OUT LABEL 'related_to' DEPTH 2;

-- Ontology: subclass inference (auto-includes subclasses)
SELECT * FROM Vehicle;  -- returns Car, Truck, Motorcycle instances too

-- Hybrid: combine modalities
SELECT p.name, VECTOR_DISTANCE(p.embedding, [0.1, 0.2]) as score
FROM Product p
WHERE p.category = 'electronics'
ORDER BY score
LIMIT 5;
```

## How It Works

1. **Single LSM-Tree** — All data types share the same storage engine
2. **Unified Key Format** — `ClassName::PrimaryKey` for all modalities
3. **Type-Aware Indexes** — B+Tree for columns, HNSW for vectors, R*-tree for spatial
4. **Query Planner** — Automatically selects the best index for each query

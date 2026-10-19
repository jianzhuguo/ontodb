# OntoDB — Ontology-Kernel Embedded Multi-Modal Semantic Database

English | **[中文](README.md)**

**The world's first multi-modal semantic database embedding an OWL reasoning engine in the storage kernel.**

Unified storage of 12 data types, 7-step write-on-insert pipeline, embedded incremental reasoning 100x faster than external solutions.

---

## Why OntoDB

**The reality for most teams**: drugs in MySQL, graphs in Neo4j, vectors in Milvus, time series in InfluxDB, reasoning in Jena — one question spans 5 systems, ETL after every write, taxonomy hard-coded in application logic. **Lots of data, little knowledge.**

OntoDB solves this in one engine:

| Dimension | Traditional | OntoDB |
|-----------|------------|--------|
| **Architecture** | MySQL + Neo4j + Milvus + InfluxDB + PostGIS + Jena (6 systems) | **1 system**, 12 data types unified |
| **Semantics** | ETL after write, seconds to minutes | **Semantic-on-Insert**, 7-step atomic pipeline, microseconds |
| **Reasoning** | External engine, data shuffling | **OWL reasoning embedded in storage kernel**, zero overhead |
| **Query** | SQL + Cypher + SPARQL + proprietary API (4 languages) | **OntoQL**, one syntax for relational + graph + vector + reasoning |
| **Semantic Metadata** | Metadata only describes table structure (`pg_catalog`); taxonomy lives in application code | **Ontology IS the metadata**: class inheritance / equivalence / disjointness are inferable; `subClassOf` queries auto-expand subclasses |
| **Deployment** | 6 independent systems, complex ops | **3-tier edge**, ESP32 to cloud server, unified architecture |
| **Multi-tenant** | App-layer tenant_id, easy to miss | **Namespace kernel isolation**, key-prefix routing, zero overhead |
| **Data Lifetime** | TTL fixed expiry, no value awareness | **Living Data decay**, data lives or dies based on access |
| **Reliability** | Luck and restarts | **Block-level CRC + guard canary**; corrupted data deterministically rejected; heap-corruption root cause closed |
| **Cost** | 6 systems x 6 ops x 6 licenses | **1 system**, 80%+ ops cost reduction |

**Production-verified** (a biomedical knowledge graph, 12 cores / 32GB / NVMe):

- **16.35M records** (8.15M entities + 8.21M relation edges) + 2.45M molecular structures, 7×24 online with zero crashes
- GROUP BY over 607K rows **123ms** (90×); large-table scan **2,546 rps** (238×)
- Querying "EGFR inhibitor" **automatically hits all subclass drugs** — new subclasses need zero code

> **In one line**: other databases store data; OntoDB makes data understandable, inferable, and verifiable.

---

## Semantic Metadata vs Traditional Metadata

Traditional database metadata (PostgreSQL's `pg_catalog` / `information_schema`) only describes **table structure** — table names, column types, indexes, constraints. Taxonomy, relationship semantics, and inference rules live in application code; the database itself cannot "understand" its data.

OntoDB embeds the **Ontology** into the storage kernel — it is not just a schema, but **inferable domain knowledge**.

### Point-by-point comparison (PostgreSQL as baseline)

| Dimension | PostgreSQL Metadata | OntoDB Semantic Metadata | Gap |
|-----------|-------------------|--------------------------|-----|
| **Storage location** | `pg_catalog` system tables (separate namespace) | Same database, same transaction | PG is an "archive cabinet"; OntoDB is "living knowledge" |
| **Describes** | Tables / columns / indexes / constraints / privileges | Concepts / properties / relations / inheritance / equivalence | Structure vs semantics |
| **Inheritance queries** | Table inheritance (`INHERITS`) **does not participate in query expansion** — manual UNION ALL | `subClassOf` queries **auto-expand subclasses** | PG relies on app-layer hardcoding |
| **Constraint types** | NOT NULL / UNIQUE / CHECK / FK | All of the above + **OWL: disjoint / inverseOf / transitive / equivalentClass** | PG constrains single tables; OWL governs cross-table semantics |
| **Reasoning** | None | **Reason-on-Insert** (incremental fixpoint, 50.8µs) | PG stores facts; OntoDB derives new ones |
| **Relationship semantics** | Foreign keys only guarantee referential integrity | `inverseOf` auto-symmetric, `transitive` auto-propagating | PG knows "references"; not "means" |
| **Cross-modal queries** | Needs pgvector / pgRouting extensions | **12 modalities natively unified** | PG is assembled; OntoDB is integrated |
| **Standards** | SQL only | SQL + OntoQL + **SPARQL / RDF** | Direct semantic-web ecosystem access |
| **Data quality gates** | Type checks on write | Required + type + OWL + **logical consistency** | PG blocks format errors; OntoDB blocks semantic contradictions |
| **Metadata consumers** | Optimizer, DDL, BI tools | Optimizer + **reasoner + validator + graph + AI** | Serves tools vs drives intelligence |

### A concrete example

Querying "all EGFR inhibitors":

```sql
-- PostgreSQL: you must maintain the subclass list yourself; new drugs mean code changes
SELECT * FROM drug
WHERE class IN ('EGFRInhibitor', 'FirstGenEGFR', 'SecondGenEGFR', 'ThirdGenEGFR');

-- OntoDB: the ontology declares subClassOf; the query auto-expands
SELECT * FROM kb.Drug WHERE class = 'EGFRInhibitor';
-- Gefitinib, erlotinib, osimertinib, almonertinib ... all hit, zero code changes for new subclasses
```

**Why PG cannot do this**: `pg_catalog` records "the drug table has a class column" — it does not record "EGFRInhibitor is a subclass of KinaseInhibitor." That knowledge lives in application code, not in the metadata.

### What an ontology definition contains

```sql
CREATE ONTOLOGY kb (
  CLASS kb.Compound,
  PROPERTY id         DOMAIN kb.Compound RANGE STRING,
  PROPERTY smiles     DOMAIN kb.Compound RANGE STRING,
  PROPERTY inchi_key  DOMAIN kb.Compound RANGE STRING,
  PROPERTY formula    DOMAIN kb.Compound RANGE STRING,
  PROPERTY mol_weight DOMAIN kb.Compound RANGE STRING
)
```

This is not just table creation — it is **inferable knowledge definition**: classes can `SUBCLASS OF` parents forming taxonomies, properties can declare `inverseOf` (inhibits ↔ is-inhibited) and `transitive`, classes can declare `disjointWith` (Drug vs Disease are mutually exclusive).

### Four core values

1. **Semantic query auto-expansion** — `subClassOf` reasoning: query a parent class, get all subclasses automatically; deeper taxonomies mean bigger wins
2. **Write-time data-quality gates** — once registered, every INSERT is validated for required fields / types / OWL constraints; semantic contradictions cannot enter the database
3. **Cross-modal semantic anchor** — graph, vector, document, and time series share one ontology; relationship semantics are inferable and evolvable, not hardcoded in queries
4. **Semantic-web & AI ecosystem** — SPARQL/RDF compatible; imports standard ontologies (ChEBI/HPO/GO); standard input for RAG / knowledge graphs / AI

> **In one line**: ordinary metadata answers "what does the table look like"; OntoDB's semantic metadata answers "what does the data mean, how does it relate, and what new knowledge can be derived."

---

## Vector Search vs Traditional Vector Databases

Traditional stacks treat vectors as isolated float arrays: Milvus / pgvector store embeddings and return Top-K, while business tables and knowledge graphs live in another world. After search you still join back to the business DB; metadata filters either pre-select IDs in SQL or rely on coarse scalar filtering inside the vector DB. **Vectors without knowledge.**

OntoDB embeds vector indexing **in the storage kernel**: vectors share the same database and transactions with documents, graph, and ontology. SQL predicates, graph relations, and ontology reasoning participate natively in search — not bolted on.

### Point-by-point comparison (Milvus / pgvector as baseline)

| Dimension | Milvus / pgvector | OntoDB Vector Capability | Essence of the Gap |
|-----------|-------------------|--------------------------|--------------------|
| **Architecture** | Separate vector DB + business DB, post-write ETL | **Same DB, same transactions** as documents / graph / ontology | Assembled vs integrated |
| **Binary fingerprints** | Usually float32; 1024-d takes 4KB | **1024-bit packed into 128B** (8× smaller), bit-op fast path | Generic embeddings vs specialized for chemistry / binary features |
| **Distance metrics** | L2 / IP / Cosine typical | L2 / Cosine / IP / **Tanimoto / Hamming** | Native chemical similarity & Hamming |
| **Filtered search** | Pre-select IDs in SQL then vector, or approximate scalar filter | **VECTOR SEARCH ... WHERE** pushed down in one plan | Two queries vs one fusion |
| **Knowledge reasoning** | None | Ontology walk after hit (subclass / transitive / equivalence) | Similarity ends at similarity |
| **Incremental maintenance** | Full rebuild or third-party index tools | **Incremental delta + full snapshot**; bulk vs online split | Ops scripts vs built into engine |
| **Metadata consistency** | Two systems; scalar filter depends on version | Vector nodes share `{Class}::PK` anchor with documents | Mapping-table risk vs zero mapping |

### A concrete example: molecular similarity search

```sql
-- Find molecules most similar to aspirin using a 1024-bit Morgan fingerprint
VECTOR SEARCH ON sembio.Compound (fp)
QUERY [1.0, 0.0, 1.0, ... ]   -- 0/1 bit vector
TOP 10
WHERE mol_weight < 500;        -- optional: SQL predicate pushed down
```

**Production-verified at 2.45M molecules** (full ChEMBL library, Tanimoto):

| Query molecule | Fingerprint bits | Latency | Top hits (excerpt) |
|----------------|------------------|---------|---------------------|
| Aspirin | 24 bits | **21ms** | Carbaspirin, salsalate, phenylaspirinate |
| Clopidogrel | 45 bits | **41ms** | Bisulfate salt, vicagrel (same-target analog) |
| Paclitaxel | 86 bits | **39ms** | Taxane side-chain / chloro analogs |

Full-library search in **< 50ms**; self-match distance 0; salts, stereoisomers, and same-scaffold analogs are recalled together.

### Capability list

1. **HNSW graph index** — hierarchical navigable small-world graph; `ef_search` tunable at query time
2. **Binary fingerprint specialization** — bit-packed storage and Tanimoto/Hamming bit-op fast paths; no 1024-dim float expansion
3. **Incremental persistence** — small writes as incremental deltas; bulk import automatically switches to full snapshot; restart-safe without full rebuild
4. **Self-healing structure** — stale-node prune (`COMPACT VECTOR INDEX`), tombstone filtering, duplicate-id collapse
5. **Cross-modal fusion** — vector + SQL filter + graph traversal + ontology reasoning in one query (OntoQL / hybrid API)
6. **Unified semantic anchor** — vector doc key, graph vertex, triple subject, and storage PK are the same `{Class}::PK`; zero mapping tables

### Four core values

1. **Search becomes knowledge** — a vector hit is not the end; follow ontology relations to targets, pathways, adverse events
2. **Designed for domain features** — native fingerprints, Hamming, Tanimoto; not just text embeddings
3. **No post-write migration** — vectors commit with business data in one transaction; no ETL window, no dual-write drift
4. **Production-ops ready** — incremental persistence, bulk import path, metrics, snapshot restore in one place

> **In one line**: other vector DBs tell you "which vectors look alike"; OntoDB tells you **which things** look alike — and what that means in a knowledge network.

---

## 12 Data Types

OntoDB unifies 12 data types in a single storage engine:

| Category | Data Type | Description |
|----------|-----------|-------------|
| **Basic** | Key-Value | High-performance read/write |
| | Document | JSON/BSON, nested structures |
| | Text | Inverted index + Chinese/English tokenizer + BM25 ranking |
| | Multimedia | Metadata registry + SHA256 content hash + type/attribute search |
| **Advanced** | Structured Records | Relational data + transactional semantics |
| | Graph | Entity-relationship network + traversal |
| | Vector Index | HNSW semantic similarity search |
| | Time Series | Sensor/monitoring/log data |
| | Spatial | R-Tree + GeoHash geographic data |
| | Semantic Triples | Knowledge graph SPO |
| **Real-time** | Stream | CDC change capture + continuous queries |
| | Event | Complex event processing |

All data linked via **unified semantic anchor** (`{Class}::{PrimaryKey}`), zero mapping tables.

---

## 33 Core Technologies

### Storage Engine

| # | Innovation | One-liner |
|---|-----------|-----------|
| 1 | **Unified Semantic Anchor** | `{Class}::PK` serves as storage key, graph vertex, vector doc key, and triple subject simultaneously |
| 2 | **7-Step Write Pipeline** | Single INSERT atomically completes document -> graph -> triple -> reasoning -> vector -> index |
| 3 | **Full-Modal Unified Storage** | LSM-Tree + key-prefix routing, 12 data types coexist in one engine |
| 4 | **Binary Row Zero-Copy Filtering** | Filter on binary data without deserialization, 60% parse time reduction |
| 5 | **3-Phase Lock Commit** | Prepare -> pre-commit -> commit, avoids write/index lock deadlock |
| 7 | **4-Layer Cache Kernel Embedding** | Block cache + query cache + semantic cache + adaptive memory, zero network overhead, shared memory |

### Reasoning Engine

| # | Innovation | One-liner |
|---|-----------|-----------|
| 7 | **Embedded Incremental Reasoning** | OWL 2 RL embedded in storage kernel, incremental fixpoint, 10-100x speedup |
| 8 | **Reasoning Safety** | Fact budget control, parallel thread pool, cycle detection |
| 9 | **Derivation Chain Tracing** | Every derived fact records source rule and input facts |

### Ontology Kernel: OWL 2 RL Reasoning Engine

OntoDB embeds the OWL 2 RL reasoning engine **inside the storage kernel**, not as an external component. Reasoning happens automatically on write, subclasses expand automatically on query, derived facts persist instantly.

```
Traditional: DB -> ETL -> external reasoning engine -> write back (minutes)
OntoDB:      Data write -> kernel auto-reasoning -> instant persist (microseconds)
```

**Supported OWL 2 RL Rules:**

| Rule | Meaning | Example |
|------|---------|---------|
| **Subclass Propagation** | Subclass instances belong to parent class | TemperatureSensor IS_A Sensor -> query "Device" includes all |
| **Equivalent Class** | Equivalent classes include each other | Employee = Worker -> query either includes both |
| **Transitivity** | Transitive relations auto-derive | A contains B, B contains C -> A contains C |
| **Symmetry** | Symmetric relations derive both ways | A interconnected B -> B interconnected A |
| **Inverse Property** | Inverse relations auto-derive | A is parent of B -> B is child of A |
| **Sub-property** | Sub-property inherits parent | "manages" is sub-property of "knows" |
| **Equivalent Property** | Equivalent properties interchange | "father" = "dad" |

**Safety:** Budget control (max facts), parallel thread pool, cycle detection, full derivation tracing.

### Query Engine

| # | Innovation | One-liner |
|---|-----------|-----------|
| 10 | **OntoQL Unified Query** | One syntax for SQL + graph traversal + vector search + ontology reasoning + temporal + spatial |
| 11 | **SPARQL Support** | Standard SPARQL 1.1, FILTER/OPTIONAL/UNION/EXISTS |
| 12 | **Inference Clause** | Real-time reasoning in queries, `SELECT * FROM Device` auto-includes all subclasses |
| 13 | **Semantic Cache** | Queries with same semantic meaning hit same cache, regardless of text differences |
| 14 | **Multi-Modal Fusion Optimizer** | Vector+relational, spatial+temporal, text+vector auto-select optimal execution plan |
| 15 | **Data Lineage** | Kernel-level provenance tracking, traces to original write + every reasoning step |
| 16 | **Federated Query** | External sources (PG/MySQL/REST/CSV/JSON) mapped as namespaces, unified query |

### OntoQL: Unified Query Language

One language for all data modalities. Traditional: SQL + Cypher + vector API + SPARQL (4 languages).

```sql
-- Relational
SELECT * FROM users WHERE age > 25;

-- Graph traversal
GRAPH TRAVERSE FROM 'Person::1' OUT LABEL 'knows' DEPTH 3;

-- Vector search
VECTOR SEARCH ON documents (embedding) QUERY [0.1, 0.2] TOP 10;

-- Ontology reasoning (auto-expand subclasses)
SELECT * FROM Device;  -- auto-includes Sensor, TemperatureSensor, etc.

-- Spatio-temporal query
SELECT * FROM pois WHERE ST_Distance(location, ST_Point(116.4, 39.9)) < 1000;
```

### Living Data Management

| # | Innovation | One-liner |
|---|-----------|-----------|
| 17 | **Living Data Lifecycle** | Exponential/linear/logarithmic decay, 3-tier half-life (7h/70d/2yr), auto-activate on access |
| 18 | **Value-Driven Query** | Filter and sort by value score, high-frequency data auto-promotes |

Traditional TTL: fixed expiry, no value awareness. Living Data: **data lives like a living thing** — unused data slowly "dies", accessed data "revives".

| Feature | Description |
|---------|-------------|
| **3-Tier Half-Life** | 7 hours (hot) -> 70 days (warm) -> 2 years (cold) |
| **3 Decay Models** | Exponential (default), linear, logarithmic |
| **Auto-Activate** | Every access resets decay clock, accumulates value score |
| **Kernel Integration** | Value metadata stored with data in same record, atomic |
| **Vector Index Integration** | High-value data vectors loaded into memory first |

### Temporal & Spatial

| # | Innovation | One-liner |
|---|-----------|-----------|
| 19 | **TSM Columnar Storage** | Timestamp Delta encoding + float Gorilla encoding, 60%+ storage reduction |
| 20 | **Spatio-Temporal Joint Index** | Quadtree spatial partition + timeline sort in single data structure |
| 21 | **Embedded STTRL Engine** | 10+ spatio-temporal rules (geofence/speed/proximity/anomaly) at database level |
| 22 | **3-Tier Storage** | Hot(memory) -> Warm(SSD) -> Cold(HDD), auto-migration by data temperature |
| 23 | **Advanced Time Series** | DTW distance, streaming anomaly detection, tumbling/hopping/session windows |

### Distributed & Scaling

| # | Innovation | One-liner |
|---|-----------|-----------|
| 24 | **Data Sharding** | Class/Range/Hash strategies, runtime scaling and rebalancing |
| 25 | **Raft Consensus** | Multi-node cluster consistency, persistent log, cluster whitelist |
| 26 | **Multi-Protocol Access** | PostgreSQL + MySQL + HTTP REST simultaneously |
| 27 | **Embedded API Gateway** | Load balancing, rate limiting, circuit breaking, auth |
| 28 | **3-Tier Edge Deployment** | MCU(<100KB) -> Embedded Linux -> Edge server, unified model and API |

### Security & Operations

| # | Innovation | One-liner |
|---|-----------|-----------|
| 29 | **CDC Change Capture** | Kernel-integrated, WAL real-time extraction, Flink/Spark compatible |
| 30 | **Namespace Kernel Isolation** | `{namespace}::{class}::{pk}` 5-layer full-stack isolation, key-prefix routing zero overhead |
| 31 | **GB/T 22239 Separation of Duties** | System/Security/Audit admins mutually exclusive, meets China gov/finance compliance |
| 32 | **Data Masking Engine** | Kernel-level dynamic + static masking, auto-detect ID card/phone/bank card patterns |
| 33 | **CRC Auto-Repair** | SSTable page-level CRC validation, auto-repair from Raft replicas on corruption |

### Namespace: Kernel-Level Multi-Tenant Isolation

OntoDB namespaces are **embedded in the storage kernel**, not application-layer tags. Entity ID `{namespace}::{class}::{pk}` IS the storage key.

```
Traditional: App adds tenant_id -> WHERE tenant_id = ? -> easy to miss
OntoDB:      EntityId = {namespace}::{class}::{pk} -> key-prefix routing -> physical isolation
```

| Isolation Layer | Description |
|----------------|-------------|
| **Ontology** | Each namespace has independent class definitions, constraints, inheritance |
| **Data** | Key-prefix `{namespace}::` physical isolation, no cross-contamination |
| **Query** | Query engine auto-applies namespace context, no WHERE needed |
| **Index** | Vector/B-Tree/Spatial indexes isolated by namespace |
| **Reasoning** | OWL reasoning scoped to namespace, no cross-tenant inference |

One OntoDB instance serves N tenants/projects with low ops cost and data security.

---

## Performance Benchmarks

> **1M+ throughput, microsecond reasoning, 100% vector recall. One system unifies 12 data types, replacing 6 independent databases.**

### Production-Verified (a biomedical knowledge graph)

> 12 cores / 32GB RAM / NVMe SSD / **16.35M records** (8.15M entities + 8.21M relation edges) / 14.8GB / 439 SSTables

| Metric | Value |
|--------|-------|
| Total records | **16.35M+** (55 entity tables + 21 relation tables) |
| Molecular structures (SMILES/InChI) | **2.45M** (ChEMBL 35) |
| **Molecular fingerprint similarity (Tanimoto)** | **21–41ms / 2.45M** |
| GROUP BY over 607K rows | **123ms** (11s before index pushdown, 90×) |
| Large-table LIMIT scan (16 concurrent) | **2,546 rps** (10.7 rps before, 238×) |
| P50 latency at 64 concurrent | **25.8ms** (8.9s before) |
| Mixed-workload soak | **180s / 69,610 queries / 0 errors** |
| Corrupted-data injection test | **All deterministically rejected, zero crashes** |

### Storage Engine

| Metric | OntoDB | SQLite | PostgreSQL | Redis |
|--------|--------|--------|-----------|-------|
| Write | **1,126,486 ops/s** | ~50K | ~30K | ~100K |
| Read | **1,306,438 ops/s** | ~200K | ~100K | ~100K |

### Vector Search

| Scale | Recall | Latency |
|-------|--------|---------|
| 10K | **100%** | **403µs** |
| 50K | **100%** | **1.03ms** |
| 200K | **99.7%** | **4.06ms** |
| **2.45M (molecular fingerprints, Tanimoto)** | — | **21–41ms** |

### OWL Reasoning (World's First Embedded Engine)

| Operation | OntoDB | External Engine |
|-----------|--------|----------------|
| is_subclass_of | **390ns** | ~10ms |
| Single fact reasoning | **50.8µs** | ~100ms |
| Batch reasoning (10K entities) | **22ms** | ~5s |

### Rule Engine

| Scale | Latency |
|-------|---------|
| 10 rules | **34.5µs** |
| 1000 rules | **1.79ms** |

### Performance vs Domain-Specific Databases

| Function | Specialist DB | Specialist Perf | OntoDB | Note |
|----------|--------------|----------------|--------|------|
| Relational | PostgreSQL | ~30K ops/s | **1,126K ops/s** | Write throughput |
| Vector search | Milvus | ~1ms / 95-99% | **403µs / 100%** | 10K scale |
| Graph query | Neo4j | ~10ms | **<1ms** | BFS/DFS |
| OWL reasoning | Apache Jena | ~100ms | **50.8µs** | Incremental fixpoint |
| Time series | InfluxDB | ~200K ops/s | **1,126K ops/s** | TSM columnar |
| Rule engine | Drools | ~500µs/10 rules | **34.5µs/10 rules** | DSL syntax |

> See [BENCHMARK_REPORT.md](BENCHMARK_REPORT.md) · [Semantic Metadata Deep Dive](docs/marketing/SEMANTIC_METADATA.md)

---

## Quick Start

### Build from Source

```bash
# Prerequisites: Rust 1.77+
git clone https://gitee.com/ontovalue/ontodb.git
cd ontodb
cargo build --release

# Start server (PostgreSQL on 5432 + MySQL on 3306 + HTTP on 7912)
./target/release/ontodb-server --data-dir ./data --http 0.0.0.0:7912
```

### Docker

```bash
docker compose up -d
```

### Create Your First Table with OntoQL

```sql
-- Define ontology
CREATE CLASS Device;
CREATE CLASS Sensor SUBCLASS OF Device;

-- Insert data (auto-completes 7-step pipeline)
INSERT INTO Sensor (id, location, temperature)
VALUES ('T1', 'Factory_A', 36.5);

-- Query auto-expands subclasses
SELECT * FROM Device;

-- Graph traversal
GRAPH TRAVERSE FROM 'Sensor::T1' OUT LABEL 'connects_to' DEPTH 3;

-- Vector search
VECTOR SEARCH ON documents (embedding) QUERY [0.1, 0.2] TOP 10;

-- Spatial query
SELECT * FROM devices WHERE ST_Distance(location, ST_Point(116.4, 39.9)) < 1000;

-- Time series query
SELECT * FROM sensor_data WHERE time > '2024-01-01' LIMIT 100;
```

---

## Use Cases

> **Market positioning: the semantic service layer of the big-data stack.** Big-data pipelines (lakehouse / Spark / Flink) handle cold data and batch analytics; OntoDB handles semantic queries and online services — ontology reasoning, unified multi-modal queries, write-time data-quality gates, and knowledge graphs — connected via CDC / import pipelines.

**Delivered proof**: biomedical knowledge graph (16.35M records) — drug / target / disease / adverse-reaction data from ChEMBL / UniProt / ClinVar in one engine. `subClassOf` reasoning makes "EGFR inhibitor" queries automatically hit all subclass drugs. Running 7×24 with zero crashes.

| Scenario | Why OntoDB |
|----------|-----------|
| **LLM / AI Companies** | Vector + knowledge graph + reasoning fused, RAG + built-in ontology |
| **Biopharma / AI4S** | Molecular structure + graph + vector + ontology unified; structure-similarity → target-pathway → adverse-event in one query (delivered) |
| **Internet Companies** | 12 data types in 1 system, replace MySQL+Redis+Milvus+ES+Neo4j |
| **SaaS Platforms** | Namespace kernel isolation, one instance for N tenants, zero overhead |
| **Knowledge Graphs** | OWL reasoning embedded, write-on-reason, traceable derivation chains |
| **Intelligent Search** | Vector + full-text + semantic reasoning fused query |
| **IoT / Industrial** | Time series + spatial + graph + ontology unified, STTRL engine built-in |
| **Digital Twin** | 12 data types + living data + STTRL + CDC real-time events |
| **Autonomous Driving** | 3-tier edge deployment + STTRL + CDC |
| **Robotics** | Edge low-power + real-time reasoning + spatial awareness + ontology rules |
| **Smart Healthcare** | Namespace multi-tenant + ontology + time series (vitals) + spatial (ward) |
| **Financial Risk** | Ontology rules auto-trigger risk controls + stream + CDC + audit chain |
| **E-Commerce** | Vector similarity + graph relations + living data decay + real-time events |
| **Smart City** | Spatial + temporal + graph + ontology, geofence + trajectory + device relations |
| **Multi-Project Data Hub** | One OntoDB instance for N projects, namespace physical isolation |
| **Edge Computing** | MCU < 100KB to full server, unified data model and API |
| **Embodied Intelligence** | Edge reasoning + spatio-temporal rules + ontology-driven rule engine |

### Industry Case Studies

| Case | Pain Point | OntoDB Solution | Impact |
|------|-----------|----------------|--------|
| **Biomedical knowledge graph** (delivered) | Drug / target / pathway / adverse-event data split across 5 systems; "EGFR inhibitor" queries miss subclasses | 12 modalities in one engine + `subClassOf` auto-expansion | 607K-row aggregate **11s→123ms**; table scan **238×** |
| **Clinical trial matching** | Eligibility criteria buried in documents; patient screening takes weeks | Ontologized eligibility + reasoning-based matching | Screening **weeks→minutes**, new trials need zero code |
| **AML transaction graph** | Fund flows split across 3 databases; link mining is manual | Transaction graph + class ontology + transitive risk reasoning | Link mining **×3**, full group-risk coverage |
| **Equipment fault knowledge graph** | Fault cases unstructured; troubleshooting relies on expert experience | Fault taxonomy ontology + causal graph + sensor time series | Troubleshooting time **-50%**, alert accuracy **+30%** |
| **Product catalog management** | 5–8-level category trees; "phone" queries need recursive UNION | `subClassOf` auto-expansion + synonym equivalence | **1 SQL** replaces recursive UNION, recall **+35%** |

> Case 1 is a production-verified delivery; cases 2–5 are reference designs with industry-baseline estimates. See [Semantic Metadata Deep Dive](docs/marketing/SEMANTIC_METADATA.md) (Chinese) for details.

### Scenario Fit

Semantic metadata delivers the most value where taxonomies are deep, cross-table joins are dense, and data quality matters:

| Scenario | Taxonomy Depth | Cross-table Links | Quality Bar | Fit |
|----------|---------------|-------------------|-------------|-----|
| Biopharma / Chemistry & Materials | ★★★★★ | ★★★★ | ★★★★ | **Highest** (verified) |
| Healthcare / FinTech | ★★★★ | ★★★★★ | ★★★★ | **High** |
| Manufacturing / E-Commerce | ★★★★ | ★★★ | ★★★ | **Medium-High** |
| Smart City / Government | ★★★★ | ★★★★ | ★★★★ | **Medium** |

---

## Project Structure

```
ontodb/
├── crates/
│   ├── onto-core/          # Core types (EntityId/BinaryRow/R-Tree/GeoHash/DTW)
│   ├── onto-storage/       # LSM-Tree engine (WAL/MVCC/HNSW/TSM/Tiered/Bloom)
│   ├── onto-ontology/      # OWL 2 RL reasoning (7 rules/incremental fixpoint)
│   ├── onto-query/         # Query engine (OntoQL/SPARQL/optimizer/executor/cache)
│   ├── onto-graph/         # Graph model (adjacency list/BFS/DFS)
│   ├── onto-server/        # Multi-protocol server (PG/MySQL/HTTP/TLS/Audit/CDC)
│   ├── onto-cli/           # CLI REPL + dump/restore
│   ├── onto-plugin/        # Plugin hook pipeline (INSERT/UPDATE/DELETE lifecycle)
│   ├── onto-edge/          # Edge device SDK (collector/geofence/reporter)
│   ├── onto-sharding/      # Data sharding (class/range/hash strategies)
│   ├── onto-raft/          # Raft consensus (persistent/whitelist/config sync)
│   └── ontodb-rules/       # Basic rules API server (DSL/CRUD/hot reload)
├── sdk/                    # Multi-language SDK (Go/Python/TypeScript)
├── examples/               # Example apps + rule templates
├── frontend/               # Rules editor Web UI
└── docs/                   # Documentation
```

---

## Enterprise Features

The Enterprise edition adds three core capabilities on top of Community:

### Ontology-Driven Rule Engine (World First)

**Ontology knowledge auto-triggers business rules. Domain experts only define ontology, rules activate automatically.** Unlike traditional rule engines that require manual rule authoring, OntoDB Enterprise fuses OWL reasoning with the rule engine to achieve "knowledge as rules."

| Capability | Description |
|-----------|-------------|
| **Ontology → Rules** | OWL reasoning results auto-inject into rule engine, no coding needed |
| **Enterprise Rule Engine** | Full DSL syntax, distributed reasoning, conflict detection, versioning, profiling |

### Security & Compliance

Meets compliance requirements for government, financial, and healthcare industries.

| Capability | Description |
|-----------|-------------|
| **Identity & Access** | RBAC separation of duties, LDAP/SAML integration |
| **Data Protection** | SM4 + AES encryption, data masking, key management |
| **Audit & Compliance** | Audit logging, compliance retention, integrity validation |

### High Availability

Enterprise-grade reliability.

| Capability | Description |
|-----------|-------------|
| **Cluster HA** | Auto failover, read/write split, rolling upgrade |
| **Distributed Query** | Cross-shard aggregation, distributed backup/restore |
| **Observability** | Slow query analysis, metrics collection, alerting |

Contact: license@ontovalue.com

---

## License

OntoDB uses a dual licensing model:

| Edition | License | Description |
|---------|---------|-------------|
| **Community** | BUSL-1.1 | Free to use, DBaaS hosting prohibited, converts to Apache-2.0 on 2031-09-15 |
| **Enterprise** | Commercial | Full features, requires purchased license |

**Community (BUSL-1.1):**
- Allowed: organizations with annual revenue below RMB 20M (~USD 2.8M) use for free
- Allowed: internal use, local deployment, secondary development, non-commercial distribution
- Allowed: enterprise self-hosting, embedding in SaaS products
- Prohibited: offering OntoDB as a cloud database service (DBaaS)
- Organizations exceeding RMB 20M annual revenue must purchase a commercial license
- Auto-converts to Apache License 2.0 on 2031-09-15
- See [LICENSE](LICENSE)

**Enterprise (Commercial):**
- Advanced reasoning engine (full DSL + OWL integration + distributed reasoning + profiling)
- Security compliance (RBAC + LDAP + SM4/AES encryption + KMS + audit retention)
- High availability (cluster failover + cross-shard query + backup/restore)
- See [LICENSE.COMMERCIAL](LICENSE.COMMERCIAL)
- Contact: license@ontovalue.com

**Compliance Notice:** Using OntoDB in violation of BUSL-1.1 terms (including but not limited to: removing copyright notices, providing DBaaS hosting services) will result in legal liability. Report violations to: license@ontovalue.com

---

## Contributing

Contributions welcome! See [CONTRIBUTING.md](CONTRIBUTING.md).
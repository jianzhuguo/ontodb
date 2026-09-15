# OntoDB — Ontology-Kernel Embedded Multi-Modal Semantic Database

English | **[中文](README.md)**

**The world's first multi-modal semantic database embedding an OWL reasoning engine in the storage kernel.**

Unified storage of 12 data types, 7-step write-on-insert pipeline, embedded incremental reasoning 100x faster than external solutions.

---

## Edition Comparison

| Feature | Community (BUSL-1.1) | Enterprise (Commercial) |
|---------|---------------------|------------------------|
| **Storage Engine** | LSM-Tree + MVCC + WAL | Same as Community |
| **12 Data Types Unified** | Yes | Yes |
| **SQL/OntoQL/SPARQL Query** | Yes | Yes |
| **HNSW Vector Index** | Yes | Yes |
| **OWL 2 RL Reasoning** | Incremental fixpoint + derivation trace | Same as Community |
| **7-Step Write Pipeline** | Yes | Yes |
| **PostgreSQL/MySQL Compatible** | Yes | Yes |
| **TLS/mTLS** | Yes | Yes |
| **Audit Chain (SHA256)** | Yes | Yes |
| **CDC Change Capture** | Yes | Yes |
| **Data Sharding** | Class/Range/Hash | Enterprise + Cross-shard Query |
| **Raft Consensus** | Yes | Yes |
| **Basic Rules API** | DSL + CRUD + Hot Reload | Yes |
| **Edge Device Support** | Yes | Yes |
| **Plugin Framework** | Yes | Yes |
| Advanced Reasoning Engine | No | Full DSL + OWL + Distributed + Profiling |
| Auto Failover | No | Yes |
| Cross-Shard Query | No | Yes |
| Slow Query Monitoring | No | Observability |
| Full Backup | No | Yes |
| RBAC Separation of Duties | No | Yes |
| LDAP/SAML | No | Yes |
| Data Masking | No | Yes |
| SM4/AES Encryption | No | Yes |
| KMS Key Management | No | Yes |
| Audit Log Rotation | No | Yes |
| CRC Validation | No | Yes |
| Rolling Upgrade | No | Yes |

---

## Why OntoDB

| Dimension | Traditional | OntoDB |
|-----------|------------|--------|
| **Architecture** | MySQL + Neo4j + Milvus + InfluxDB + PostGIS + Jena (6 systems) | **1 system**, 12 data types unified |
| **Semantics** | ETL after write, seconds to minutes | **Semantic-on-Insert**, 7-step atomic pipeline, microseconds |
| **Reasoning** | External engine, data shuffling | **OWL reasoning embedded in storage kernel**, zero overhead |
| **Query** | SQL + Cypher + SPARQL + proprietary API (4 languages) | **OntoQL**, one syntax for relational + graph + vector + reasoning |
| **Deployment** | 6 independent systems, complex ops | **3-tier edge**, ESP32 to cloud server, unified architecture |
| **Multi-tenant** | App-layer tenant_id, easy to miss | **Namespace kernel isolation**, key-prefix routing, zero overhead |
| **Data Lifetime** | TTL fixed expiry, no value awareness | **Living Data decay**, data lives or dies based on access |
| **Cost** | 6 systems x 6 ops x 6 licenses | **1 system**, 80%+ ops cost reduction |

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

## 29 Core Technologies

### Storage Engine

| # | Innovation | One-liner |
|---|-----------|-----------|
| 1 | **Unified Semantic Anchor** | `{Class}::PK` serves as storage key, graph vertex, vector doc key, and triple subject simultaneously |
| 2 | **7-Step Write Pipeline** | Single INSERT atomically completes document -> graph -> triple -> reasoning -> vector -> index |
| 3 | **Full-Modal Unified Storage** | LSM-Tree + key-prefix routing, 12 data types coexist in one engine |
| 4 | **Binary Row Zero-Copy Filtering** | Filter on binary data without deserialization, 60% parse time reduction |
| 5 | **3-Phase Lock Commit** | Prepare -> pre-commit -> commit, avoids write/index lock deadlock |

### Reasoning Engine

| # | Innovation | One-liner |
|---|-----------|-----------|
| 6 | **Embedded Incremental Reasoning** | OWL 2 RL embedded in storage kernel, incremental fixpoint, 10-100x speedup |
| 7 | **Reasoning Safety** | Fact budget control, parallel thread pool, cycle detection |
| 8 | **Derivation Chain Tracing** | Every derived fact records source rule and input facts |

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
| 9 | **OntoQL Unified Query** | One syntax for SQL + graph traversal + vector search + ontology reasoning + temporal + spatial |
| 10 | **SPARQL Support** | Standard SPARQL 1.1, FILTER/OPTIONAL/UNION/EXISTS |
| 11 | **Inference Clause** | Real-time reasoning in queries, `SELECT * FROM Device` auto-includes all subclasses |
| 12 | **Semantic Cache** | Queries with same semantic meaning hit same cache, regardless of text differences |
| 13 | **Multi-Modal Fusion Optimizer** | Vector+relational, spatial+temporal, text+vector auto-select optimal execution plan |
| 14 | **Data Lineage** | Kernel-level provenance tracking, traces to original write + every reasoning step |
| 15 | **Federated Query** | External sources (PG/MySQL/REST/CSV/JSON) mapped as namespaces, unified query |

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
| 16 | **Living Data Lifecycle** | Exponential/linear/logarithmic decay, 3-tier half-life (7h/70d/2yr), auto-activate on access |
| 17 | **Value-Driven Query** | Filter and sort by value score, high-frequency data auto-promotes |

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
| 18 | **TSM Columnar Storage** | Timestamp Delta encoding + float Gorilla encoding, 60%+ storage reduction |
| 19 | **Spatio-Temporal Joint Index** | Quadtree spatial partition + timeline sort in single data structure |
| 20 | **Embedded STTRL Engine** | 10+ spatio-temporal rules (geofence/speed/proximity/anomaly) at database level |
| 21 | **3-Tier Storage** | Hot(memory) -> Warm(SSD) -> Cold(HDD), auto-migration by data temperature |
| 22 | **Advanced Time Series** | DTW distance, streaming anomaly detection, tumbling/hopping/session windows |

### Distributed & Scaling

| # | Innovation | One-liner |
|---|-----------|-----------|
| 23 | **Data Sharding** | Class/Range/Hash strategies, runtime scaling and rebalancing |
| 24 | **Raft Consensus** | Multi-node cluster consistency, persistent log, cluster whitelist |
| 25 | **Multi-Protocol Access** | PostgreSQL + MySQL + HTTP REST simultaneously |
| 26 | **Embedded API Gateway** | Load balancing, rate limiting, circuit breaking, auth |
| 27 | **3-Tier Edge Deployment** | MCU(<100KB) -> Embedded Linux -> Edge server, unified model and API |

### Security & Operations

| # | Innovation | One-liner |
|---|-----------|-----------|
| 28 | **CDC Change Capture** | Kernel-integrated, WAL real-time extraction, Flink/Spark compatible |
| 29 | **Namespace Kernel Isolation** | `{namespace}::{class}::{pk}` 5-layer full-stack isolation, key-prefix routing zero overhead |

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

> Environment: Windows x86_64, Rust 1.77+, Release mode (opt-level=3, LTO)
> See [benchmark-report.md](docs/benchmark-report.md) for full details

### Storage Engine

| Metric | Value |
|--------|-------|
| Write throughput | **1,126,486 ops/s** |
| Read throughput | **1,306,438 ops/s** |
| Group commit (1 thread) | **918,527 ops/s** |

### HNSW Vector Search

| Scale | Recall | Latency |
|-------|--------|---------|
| 10K vectors | **100%** | **403µs** |
| 50K vectors | **100%** | **1.03ms** |
| 200K vectors | **99.7%** | **4.06ms** |

### OWL Reasoning

| Scenario | Value |
|----------|-------|
| Single fact reasoning | **50.8µs** |
| Batch reasoning (24 facts) | **106µs** -> 46 derived |
| Batch reasoning (10K entities) | **22ms** -> 10K derived |
| is_subclass_of | **390ns** |
| Transitive chain (200 nodes) | **16ms** -> 19,900 derived |
| Incremental vs full | **10-100x** |

### Rule Engine

| Scale | Latency |
|-------|---------|
| 10 rules | **34.5µs** |
| 100 rules | **217.6µs** |
| 1000 rules | **1.79ms** |
| Distributed (4 workers, 1000 rules) | **4ms** |

### Raft Consensus

| Operation | Throughput |
|-----------|-----------|
| Log append | **37K ops/s** |
| State machine apply | **1.48M ops/s** |
| Restart recovery (50K entries) | **9.1µs** |

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

| Scenario | Why OntoDB |
|----------|-----------|
| **LLM / AI Companies** | Vector + knowledge graph + reasoning fused, RAG + built-in ontology |
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

Traditional rule engines require manual rule authoring. OntoDB Enterprise fuses OWL reasoning with the rule engine: **ontology knowledge auto-triggers business rules. Domain experts only define ontology, rules activate automatically.**

```
Traditional: Domain expert -> find developer -> write rule code -> test -> deploy (weeks)
OntoDB:      Domain expert -> define ontology (OWL) -> reasoning auto-triggers rules -> instant (minutes)
```

| Capability | Description |
|-----------|-------------|
| **OWL Integration** | Reasoning results auto-inject into rule engine, ontology changes auto-trigger re-evaluation |
| **Full DSL Syntax** | AND/OR/nested/time window(FOR 5min)/wildcard/units |
| **Distributed Reasoning** | Multi-thread parallel, 4 sharding strategies, 1000 rules < 2ms |
| **Performance Profiling** | Per-rule avg/p99/p95 latency, slow rule alerting |
| **Rule Versioning** | 50 versions per rule, rollback support |
| **Conflict Detection** | SameAttribute + PriorityOverride auto-detection |

### Security & Compliance

| Capability | Description |
|-----------|-------------|
| **RBAC Separation of Duties** | System admin / Security admin / Audit admin |
| **LDAP/SAML** | Enterprise directory integration |
| **Data Masking** | Dynamic + static masking |
| **SM4/AES Encryption** | SM4 national standard + AES-256 storage encryption |
| **KMS Key Management** | External key management service integration |
| **Audit Log Rotation** | Compliance retention + auto-cleanup |
| **CRC Validation** | SSTable page-level integrity check |

### High Availability

| Capability | Description |
|-----------|-------------|
| **Auto Failover** | Raft consensus + replica management + read/write split routing |
| **Cross-Shard Query** | Distributed aggregation, cross-shard JOIN |
| **Full Backup** | Consistent snapshot + compression + checksum |
| **Observability** | Slow query analysis + metrics collection + alerting |
| **Rolling Upgrade** | Cross-version compatibility, zero downtime |

Contact: license@ontovalue.com

---

## License

OntoDB uses a dual licensing model:

| Edition | License | Description |
|---------|---------|-------------|
| **Community** | BUSL-1.1 | Free to use, DBaaS hosting prohibited, converts to Apache-2.0 on 2031-09-15 |
| **Enterprise** | Commercial | Full features, requires purchased license |

**Community (BUSL-1.1):**
- Allowed: internal use, local deployment, secondary development, non-commercial distribution
- Allowed: enterprise self-hosting, embedding in SaaS products
- Prohibited: offering OntoDB as a cloud database service (DBaaS)
- Auto-converts to Apache License 2.0 on 2031-09-15
- See [LICENSE](LICENSE)

**Enterprise (Commercial):**
- Advanced reasoning engine (full DSL + OWL integration + distributed reasoning + profiling)
- Security compliance (RBAC + LDAP + SM4/AES encryption + KMS + audit retention)
- High availability (cluster failover + cross-shard query + backup/restore)
- See [LICENSE.COMMERCIAL](LICENSE.COMMERCIAL)
- Contact: license@ontovalue.com

---

## Contributing

Contributions welcome! See [CONTRIBUTING.md](CONTRIBUTING.md).
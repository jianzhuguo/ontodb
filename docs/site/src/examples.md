# Examples

OntoDB ships with three example applications demonstrating different use cases.

## RAG Application

**Location:** `examples/rag-app/`

Retrieval-Augmented Generation using vector search + ontology reasoning. Demonstrates:
- Creating vector indexes
- Ingesting documents with embeddings
- Similarity search with semantic filtering
- Category-based filtering using ontology

```bash
cd examples/rag-app
python main.py
```

## Knowledge Graph

**Location:** `examples/knowledge-graph/`

Property graph operations and SPARQL queries. Demonstrates:
- Creating vertices and edges
- BFS/DFS traversal
- Shortest path queries
- Filtered traversal by edge label

```bash
cd examples/knowledge-graph
python main.py
```

## Multi-Modal Query

**Location:** `examples/multi-modal/`

E-commerce catalog combining all four query modes. Demonstrates:
- SQL for structured queries (price filters, aggregations)
- Vector search for semantic similarity
- Graph traversal for product relationships
- Hybrid queries (SQL filter + vector ranking)

```bash
cd examples/multi-modal
python main.py
```

## Running the examples

All examples require a running OntoDB server:

```bash
# Start server
ontodb-server --http 127.0.0.1:7912

# Or with Docker
docker run -d -p 7912:7912 ontodb/ontodb:latest
```

Each example creates its own schema and cleans up after itself.

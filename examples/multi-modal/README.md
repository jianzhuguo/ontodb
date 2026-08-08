# Multi-Modal Query Example

Combining SQL, vector search, and graph queries in a single application.

## What it demonstrates

- SQL for structured data queries
- Vector search for semantic similarity
- Graph traversal for relationship discovery
- Hybrid queries combining multiple modalities

## Prerequisites

- OntoDB server running (`ontodb-server --http 127.0.0.1:7912`)
- Python 3.8+ with `requests` installed

## Run

```bash
python main.py
```

## Scenario

An e-commerce product catalog where:
- Products have structured data (name, price, category)
- Products have vector embeddings (for semantic search)
- Products are connected via a graph (related products, same brand, etc.)

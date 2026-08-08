# RAG Application Example

Retrieval-Augmented Generation using OntoDB's vector search + ontology reasoning.

## What it demonstrates

- Creating a vector index for document embeddings
- Ingesting documents with embeddings
- Performing similarity search with semantic filtering
- Using ontology reasoning to enrich results

## Prerequisites

- OntoDB server running (`ontodb-server --http 127.0.0.1:7912`)
- Python 3.8+ with `requests` installed

## Setup

```bash
cd examples/rag-app
pip install requests
```

## Run

```bash
python main.py
```

## How it works

1. Creates a `Document` class with a 128-dim vector index
2. Ingests sample documents (knowledge base articles)
3. For a query, generates a simple embedding (placeholder for real embedding model)
4. Searches for similar documents using vector search
5. Uses ontology reasoning to filter by document category
6. Returns the most relevant documents as context for LLM

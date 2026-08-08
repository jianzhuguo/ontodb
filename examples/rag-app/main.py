"""
RAG (Retrieval-Augmented Generation) Example using OntoDB.

Demonstrates vector search + ontology reasoning for document retrieval.
"""

import requests
import json
import hashlib
import struct

BASE_URL = "http://localhost:7912"

def api_post(endpoint, data):
    """Make a POST request to the OntoDB API."""
    resp = requests.post(f"{BASE_URL}{endpoint}", json=data)
    result = resp.json()
    if not result.get("success"):
        print(f"Error: {result.get('error')}")
        return None
    return result

def api_get(endpoint):
    """Make a GET request to the OntoDB API."""
    resp = requests.get(f"{BASE_URL}{endpoint}")
    return resp.json()

def text_to_embedding(text, dim=128):
    """Generate a deterministic embedding from text (placeholder for real model).
    In production, use a real embedding model like OpenAI ada-002 or sentence-transformers.
    """
    hash_bytes = hashlib.sha256(text.encode()).digest()
    # Repeat hash to fill dimension
    raw = (hash_bytes * (dim // len(hash_bytes) + 1))[:dim * 4]
    values = struct.unpack(f'{dim}f', raw[:dim * 4])
    # Normalize to [-1, 1]
    max_abs = max(abs(v) for v in values) or 1.0
    return [v / max_abs for v in values]

def main():
    print("=== OntoDB RAG Example ===\n")

    # Step 1: Create schema
    print("1. Creating schema...")
    api_post("/api/query", {
        "query": "CREATE CLASS Document (id STRING, title STRING, content STRING, category STRING)"
    })

    # Step 2: Create vector index
    print("2. Creating vector index...")
    api_post("/api/query", {
        "query": "CREATE VECTOR INDEX ON Document (embedding) DIM 128 METRIC cosine"
    })

    # Step 3: Ingest documents
    print("3. Ingesting documents...")
    documents = [
        {"id": "doc1", "title": "What is a Graph Database?",
         "content": "A graph database stores data in nodes and edges, representing entities and their relationships.",
         "category": "database"},
        {"id": "doc2", "title": "Vector Search Explained",
         "content": "Vector search finds similar items by comparing high-dimensional embeddings using distance metrics.",
         "category": "search"},
        {"id": "doc3", "title": "Ontology Reasoning",
         "content": "Ontology reasoning infers new knowledge from existing facts using logical rules like subclass propagation.",
         "category": "semantic"},
        {"id": "doc4", "title": "LSM-Tree Storage",
         "content": "LSM-Tree is a write-optimized storage structure that buffers writes in memory and flushes to sorted files on disk.",
         "category": "database"},
        {"id": "doc5", "title": "Hybrid Search",
         "content": "Hybrid search combines keyword matching with semantic vector search for better relevance.",
         "category": "search"},
    ]

    for doc in documents:
        embedding = text_to_embedding(doc["content"])
        embedding_str = ", ".join(f"{v:.6f}" for v in embedding[:8]) + ", ..."  # show first 8
        print(f"   Ingesting: {doc['title']} (embedding: [{embedding_str}])")

        api_post("/api/query", {"query": f'INSERT INTO Document SET id = "{doc["id"]}", title = "{doc["title"]}", content = "{doc["content"]}", category = "{doc["category"]}"'})

        # Insert vector separately
        embedding_full = ", ".join(f"{v:.6f}" for v in embedding)
        api_post("/api/query", {
            "query": f'VECTOR INSERT ON Document ({doc["id"]}) embedding [{embedding_full}]'
        })

    # Step 4: Query
    print("\n4. Querying...")
    query = "How does vector search work?"
    query_embedding = text_to_embedding(query)
    embedding_str = ", ".join(f"{v:.6f}" for v in query_embedding)

    print(f"   Query: '{query}'")
    print(f"   Searching for similar documents...\n")

    # Vector search
    result = api_post("/api/vector/search", {
        "class": "Document",
        "column": "embedding",
        "query_vector": query_embedding,
        "top_k": 3,
    })

    if result and result.get("data"):
        print("5. Results (top 3 most relevant documents):")
        for i, row in enumerate(result["data"], 1):
            print(f"   {i}. {row.get('title', 'N/A')}")
            print(f"      Category: {row.get('category', 'N/A')}")
            print(f"      Content: {row.get('content', 'N/A')[:100]}...")
            print()

    # Step 5: Semantic filter using ontology
    print("6. Filtering by category 'database' using ontology...")
    filtered_result = api_post("/api/vector/search", {
        "class": "Document",
        "column": "embedding",
        "query_vector": query_embedding,
        "top_k": 3,
        "filter": 'category = "database"',
    })

    if filtered_result and filtered_result.get("data"):
        print("   Filtered results:")
        for i, row in enumerate(filtered_result["data"], 1):
            print(f"   {i}. {row.get('title', 'N/A')} ({row.get('category', 'N/A')})")

    print("\n=== RAG Example Complete ===")

if __name__ == "__main__":
    main()

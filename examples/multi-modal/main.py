"""
Multi-Modal Query Example using OntoDB.

Combines SQL + Vector Search + Graph queries for an e-commerce catalog.
"""

import requests
import hashlib
import struct

BASE_URL = "http://localhost:7912"

def api_post(endpoint, data):
    resp = requests.post(f"{BASE_URL}{endpoint}", json=data)
    return resp.json()

def text_to_embedding(text, dim=64):
    """Deterministic embedding from text (placeholder)."""
    hash_bytes = hashlib.sha256(text.encode()).digest()
    raw = (hash_bytes * (dim // len(hash_bytes) + 1))[:dim * 4]
    values = struct.unpack(f'{dim}f', raw[:dim * 4])
    max_abs = max(abs(v) for v in values) or 1.0
    return [v / max_abs for v in values]

def main():
    print("=== OntoDB Multi-Modal Query Example ===\n")

    # Step 1: Create schema
    print("1. Creating product schema...")
    api_post("/api/query", {
        "query": "CREATE CLASS Product (id STRING, name STRING, price FLOAT, category STRING, brand STRING)"
    })
    api_post("/api/query", {
        "query": "CREATE VECTOR INDEX ON Product (embedding) DIM 64 METRIC cosine"
    })

    # Step 2: Ingest products
    print("2. Ingesting products...")
    products = [
        {"id": "p1", "name": "Wireless Headphones Pro", "price": 299.99, "category": "electronics", "brand": "AudioMax"},
        {"id": "p2", "name": "Bluetooth Speaker Mini", "price": 79.99, "category": "electronics", "brand": "AudioMax"},
        {"id": "p3", "name": "Running Shoes Ultra", "price": 149.99, "category": "sports", "brand": "SpeedFit"},
        {"id": "p4", "name": "Yoga Mat Premium", "price": 49.99, "category": "sports", "brand": "FlexGear"},
        {"id": "p5", "name": "Smart Watch Series 5", "price": 399.99, "category": "electronics", "brand": "TechWear"},
        {"id": "p6", "name": "Fitness Tracker Band", "price": 59.99, "category": "electronics", "brand": "TechWear"},
        {"id": "p7", "name": "Hiking Boots Waterproof", "price": 189.99, "category": "sports", "brand": "SpeedFit"},
        {"id": "p8", "name": "Noise Cancelling Earbuds", "price": 199.99, "category": "electronics", "brand": "AudioMax"},
    ]

    for p in products:
        embedding = text_to_embedding(p["name"])
        emb_str = ", ".join(f"{v:.4f}" for v in embedding)
        api_post("/api/query", {
            "query": f'INSERT INTO Product SET id = "{p["id"]}", name = "{p["name"]}", price = {p["price"]}, category = "{p["category"]}", brand = "{p["brand"]}"'
        })

    # Step 3: Create graph relationships
    print("3. Building product relationship graph...")
    relationships = [
        ("p1", "p2", "SAME_BRAND"),
        ("p1", "p8", "SIMILAR_TO"),
        ("p2", "p8", "SIMILAR_TO"),
        ("p3", "p7", "SAME_BRAND"),
        ("p5", "p6", "SAME_BRAND"),
        ("p1", "p5", "FREQUENTLY_BOUGHT_WITH"),
        ("p3", "p4", "FREQUENTLY_BOUGHT_WITH"),
    ]

    for i, (frm, to, label) in enumerate(relationships):
        api_post("/api/graph/edge", {
            "id": f"rel_{i}",
            "from": frm,
            "to": to,
            "label": label,
        })

    print(f"   Created {len(relationships)} relationships")

    # Step 4: SQL query — price filter
    print("\n4. SQL: Products under $100...")
    result = api_post("/api/query", {"query": "SELECT name, price, category FROM Product WHERE price < 100"})
    if result.get("data"):
        for row in result["data"]:
            print(f"   - {row.get('name')}: ${row.get('price')} ({row.get('category')})")

    # Step 5: Vector search — semantic similarity
    print("\n5. Vector search: 'audio headphones'...")
    query_vec = text_to_embedding("audio headphones")
    result = api_post("/api/vector/search", {
        "class": "Product",
        "column": "embedding",
        "query_vector": query_vec,
        "top_k": 3,
    })
    if result.get("data"):
        for row in result["data"]:
            print(f"   - {row.get('name')} (${row.get('price')})")

    # Step 6: Graph traversal — related products
    print("\n6. Graph: Products related to 'Wireless Headphones Pro' (p1)...")
    result = api_post("/api/graph/traverse", {
        "start": "p1",
        "direction": "both",
        "max_depth": 2,
    })
    if result.get("data"):
        for v in result["data"].get("vertices", []):
            print(f"   - {v.get('id')}: {v.get('properties', {}).get('name', '?')}")

    # Step 7: Hybrid — SQL filter + vector search
    print("\n7. Hybrid: 'wireless audio' in electronics category...")
    result = api_post("/api/hybrid/query", {
        "sql_filter": 'SELECT * FROM Product WHERE category = "electronics"',
        "vector_column": "embedding",
        "query_vector": text_to_embedding("wireless audio"),
        "top_k": 3,
        "class": "Product",
    })
    if result.get("data"):
        for row in result["data"]:
            print(f"   - {row.get('name')}: ${row.get('price')} ({row.get('brand')})")

    # Step 8: Schema introspection
    print("\n8. Schema introspection...")
    result = requests.get(f"{BASE_URL}/api/schema").json()
    if result.get("data"):
        print(f"   Schema: {json.dumps(result['data'], indent=2)[:500]}")

    print("\n=== Multi-Modal Example Complete ===")

if __name__ == "__main__":
    import json
    main()

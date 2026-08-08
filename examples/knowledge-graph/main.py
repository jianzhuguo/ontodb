"""
Knowledge Graph Example using OntoDB.

Demonstrates property graph CRUD, traversal, and SPARQL queries.
"""

import requests
import json

BASE_URL = "http://localhost:7912"

def api_post(endpoint, data):
    resp = requests.post(f"{BASE_URL}{endpoint}", json=data)
    result = resp.json()
    if not result.get("success") and "error" in result:
        print(f"  Error: {result['error']}")
    return result

def main():
    print("=== OntoDB Knowledge Graph Example ===\n")

    # Step 1: Create vertices
    print("1. Creating vertices...")
    vertices = [
        {"id": "alice", "labels": ["Person"], "properties": {"name": "Alice", "age": 30}},
        {"id": "bob", "labels": ["Person"], "properties": {"name": "Bob", "age": 25}},
        {"id": "charlie", "labels": ["Person"], "properties": {"name": "Charlie", "age": 35}},
        {"id": "dave", "labels": ["Person"], "properties": {"name": "Dave", "age": 28}},
        {"id": "acme", "labels": ["Company"], "properties": {"name": "Acme Corp", "industry": "Tech"}},
        {"id": "techstart", "labels": ["Company"], "properties": {"name": "TechStart", "industry": "AI"}},
    ]

    for v in vertices:
        result = api_post("/api/graph/vertex", v)
        print(f"   Added vertex: {v['id']} ({', '.join(v['labels'])})")

    # Step 2: Create edges
    print("\n2. Creating edges...")
    edges = [
        {"id": "e1", "from": "alice", "to": "bob", "label": "KNOWS", "properties": {"since": 2020}},
        {"id": "e2", "from": "alice", "to": "charlie", "label": "KNOWS", "properties": {"since": 2018}},
        {"id": "e3", "from": "alice", "to": "techstart", "label": "WORKS_AT"},
        {"id": "e4", "from": "bob", "to": "acme", "label": "WORKS_AT"},
        {"id": "e5", "from": "charlie", "to": "acme", "label": "WORKS_AT"},
        {"id": "e6", "from": "dave", "to": "bob", "label": "KNOWS"},
    ]

    for e in edges:
        api_post("/api/graph/edge", e)
        print(f"   Added edge: {e['from']} --{e['label']}--> {e['to']}")

    # Step 3: Get vertex details
    print("\n3. Getting vertex details...")
    for vid in ["alice", "acme"]:
        resp = requests.get(f"{BASE_URL}/api/graph/vertex/{vid}")
        v = resp.json().get("data", {})
        print(f"   {v.get('id')}: labels={v.get('labels')}, props={v.get('properties')}")

    # Step 4: Get neighbors
    print("\n4. Getting neighbors...")
    resp = requests.get(f"{BASE_URL}/api/graph/neighbors/alice")
    data = resp.json().get("data", {})
    print(f"   Alice's neighbors ({data.get('count', 0)}):")
    for n in data.get("neighbors", []):
        print(f"     - {n.get('id')} ({n.get('properties', {}).get('name', '?')})")

    # Step 5: BFS traversal
    print("\n5. BFS traversal from Alice (2 hops)...")
    result = api_post("/api/graph/traverse", {
        "start": "alice",
        "direction": "out",
        "max_depth": 2,
        "algorithm": "bfs",
    })
    if result.get("data"):
        data = result["data"]
        print(f"   Visited {data.get('visited_count', 0)} vertices:")
        for v in data.get("vertices", []):
            print(f"     - {v.get('id')} ({v.get('properties', {}).get('name', '?')})")

    # Step 6: Shortest path
    print("\n6. Shortest path from Alice to Dave...")
    result = api_post("/api/graph/shortest-path", {
        "from": "alice",
        "to": "dave",
        "max_depth": 5,
    })
    if result.get("data"):
        data = result["data"]
        if data.get("found"):
            print(f"   Path found (length {data['length']}): {' -> '.join(data['path']['vertex_ids'])}")
        else:
            print(f"   No path found: {data.get('message')}")

    # Step 7: Filtered traversal — only KNOWS edges
    print("\n7. Traversal filtered by 'KNOWS' edges...")
    result = api_post("/api/graph/traverse", {
        "start": "alice",
        "direction": "out",
        "max_depth": 2,
        "edge_label": "KNOWS",
    })
    if result.get("data"):
        data = result["data"]
        print(f"   Alice's social network ({data.get('visited_count', 0)} people):")
        for v in data.get("vertices", []):
            print(f"     - {v.get('properties', {}).get('name', v.get('id'))}")

    # Step 8: SQL query over graph data
    print("\n8. SQL query: All people older than 27...")
    result = api_post("/api/query", {"query": 'SELECT * FROM Person WHERE age > 27'})
    if result.get("data"):
        for row in result["data"]:
            print(f"   - {row.get('name')} (age {row.get('age')})")

    print("\n=== Knowledge Graph Example Complete ===")

if __name__ == "__main__":
    main()

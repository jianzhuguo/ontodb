# Knowledge Graph Example

Property graph operations and SPARQL queries using OntoDB.

## What it demonstrates

- Creating a property graph with vertices and edges
- Graph traversal (BFS/DFS)
- Shortest path queries
- SPARQL queries over graph data
- Combining graph + SQL queries

## Prerequisites

- OntoDB server running (`ontodb-server --http 127.0.0.1:7912`)
- Python 3.8+ with `requests` installed

## Run

```bash
python main.py
```

## Graph Structure

```
Alice --KNOWS--> Bob --WORKS_AT--> Acme Corp
  |                |
  +--KNOWS--> Charlie --WORKS_AT--> Acme Corp
  |
  +--WORKS_AT--> TechStart

Dave --KNOWS--> Bob
```

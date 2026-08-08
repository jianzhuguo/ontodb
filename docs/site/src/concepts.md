# Concepts

OntoDB is a **multi-modal database** that unifies four query paradigms in a single engine.

## The four modes

```
┌─────────────────────────────────────────────────────────────┐
│                         OntoDB                               │
│                                                              │
│   ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌──────────┐  │
│   │   SQL    │  │  SPARQL  │  │  Vector  │  │  Graph   │  │
│   │  Engine  │  │  Engine  │  │  Search  │  │  Travers.│  │
│   └────┬─────┘  └────┬─────┘  └────┬─────┘  └────┬─────┘  │
│        │             │             │             │          │
│        └─────────────┼─────────────┼─────────────┘          │
│                      │                                      │
│              ┌───────┴───────┐                              │
│              │  Ontology     │                              │
│              │  Reasoner     │                              │
│              └───────┬───────┘                              │
│                      │                                      │
│              ┌───────┴───────┐                              │
│              │  LSM-Tree     │                              │
│              │  Storage      │                              │
│              └───────────────┘                              │
└─────────────────────────────────────────────────────────────┘
```

## How they work together

- **SQL** for structured queries: `SELECT * FROM Product WHERE price > 100`
- **Vector search** for semantic similarity: find products similar to a query embedding
- **Graph** for relationships: traverse "products frequently bought together"
- **SPARQL** for semantic queries: query using RDF triples and OWL reasoning
- **Hybrid**: combine SQL filters with vector ranking, or graph traversal with SQL predicates

## Ontology reasoning

The ontology reasoner is what makes OntoDB unique. It sits between the query layer and storage, automatically:

- **Subclass propagation**: if `Dog` is a subclass of `Animal`, querying for `Animal` also returns `Dog` instances
- **Property inference**: if `hasParent` is inverse of `hasChild`, querying `?x hasParent ?y` also returns `?y hasChild ?x`
- **Restriction validation**: OWL restrictions (cardinality, value constraints) are enforced on write

See [Ontology Reasoning](./ontology.md) for details.

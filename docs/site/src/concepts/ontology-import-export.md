# Ontology Import/Export

OntoDB supports multiple RDF formats for ontology interchange.

## Turtle Format

Turtle (Terse RDF Triple Language) is the most human-readable format.

### Import

```bash
curl -X POST http://localhost:7912/api/query \
  -H "Content-Type: application/json" \
  -d '{
    "query": "IMPORT TURTLE \"@prefix ex: <http://example.org/> . ex:Product a ex:Class .\""
  }'
```

### Turtle Syntax

```turtle
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix ex: <http://example.org/> .

ex:Product a owl:Class ;
  rdfs:label "Product" .

ex:price a owl:DatatypeProperty ;
  rdfs:domain ex:Product ;
  rdfs:range xsd:float .
```

## N-Triples Export

N-Triples is a simple line-based format.

```bash
curl -X POST http://localhost:7912/api/query \
  -H "Content-Type: application/json" \
  -d '{"query": "EXPORT NTRIPLES"}'
```

Output:
```
<http://example.org/Product> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://www.w3.org/2002/07/owl#Class> .
<http://example.org/price> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://www.w3.org/2002/07/owl#DatatypeProperty> .
```

## JSON-LD Export

JSON-LD is JSON-based and easy to parse programmatically.

```bash
curl -X POST http://localhost:7912/api/query \
  -H "Content-Type: application/json" \
  -d '{"query": "EXPORT JSONLD"}'
```

Output:
```json
{
  "@context": {
    "rdf": "http://www.w3.org/1999/02/22-rdf-syntax-ns#",
    "rdfs": "http://www.w3.org/2000/01/rdf-schema#",
    "owl": "http://www.w3.org/2002/07/owl#"
  },
  "@graph": [
    {
      "@id": "ex:Product",
      "@type": "owl:Class",
      "rdfs:label": "Product"
    }
  ]
}
```

## Prefix Resolution

OntoDB resolves `@prefix` declarations automatically:

| Prefix | IRI |
|--------|-----|
| `rdf:` | `http://www.w3.org/1999/02/22-rdf-syntax-ns#` |
| `rdfs:` | `http://www.w3.org/2000/01/rdf-schema#` |
| `owl:` | `http://www.w3.org/2002/07/owl#` |
| `xsd:` | `http://www.w3.org/2001/XMLSchema#` |

## Triple Store

OntoDB stores RDF triples with three indexes for efficient lookup:

| Index | Lookup | Use Case |
|-------|--------|----------|
| SPO | Subject → Predicate → Object | "What properties does X have?" |
| POS | Predicate → Object → Subject | "Who has property Y = Z?" |
| OSP | Object → Subject → Predicate | "What connects to X?" |

### Triple Operations

```sql
-- Insert triple
INSERT TRIPLE (ex:Product1, rdf:type, ex:Product)

-- Select triples
SELECT TRIPLE ?s ?p ?o WHERE (?s, rdf:type, ex:Product)

-- Delete triple
DELETE TRIPLE (ex:Product1, rdf:type, ex:Product)
```

## SPARQL Integration

OntoDB translates SPARQL to SQL internally, so you can query RDF data with SPARQL syntax:

```sparql
PREFIX ex: <http://example.org/>

SELECT ?product ?price
WHERE {
  ?product rdf:type ex:Product .
  ?product ex:price ?price .
  FILTER (?price > 100)
}
```

This is translated to SQL and executed through the standard query engine.

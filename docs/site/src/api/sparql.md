# SPARQL Reference

OntoDB supports W3C SPARQL for querying RDF data.

## SELECT

```sparql
SELECT ?name ?age WHERE {
  ?x <name> ?name .
  ?x <age> ?age .
}
```

## FILTER

```sparql
SELECT ?name WHERE {
  ?x <name> ?name .
  ?x <age> ?age .
  FILTER (?age > 25)
}
```

## OPTIONAL

```sparql
SELECT ?name ?email WHERE {
  ?x <name> ?name .
  OPTIONAL { ?x <email> ?email }
}
```

## UNION

```sparql
SELECT ?name WHERE {
  { ?x <type> "Person" . ?x <name> ?name }
  UNION
  { ?x <type> "Organization" . ?x <name> ?name }
}
```

## EXISTS / NOT EXISTS

```sparql
SELECT ?name WHERE {
  ?x <name> ?name .
  FILTER EXISTS { ?x <age> ?age . FILTER (?age > 30) }
}
```

## LIMIT / OFFSET

```sparql
SELECT ?name WHERE {
  ?x <name> ?name .
}
LIMIT 10
OFFSET 20
```

## How it works

1. SPARQL queries are parsed by the SPARQL parser
2. Translated to SQL internally
3. Executed through the standard query engine
4. Results formatted as W3C SPARQL Results JSON

## HTTP API

```bash
curl -X POST http://localhost:7912/sparql \
  -H "Content-Type: application/json" \
  -d '{"query": "SELECT ?name WHERE { ?x <name> ?name }"}'
```

Response format follows [W3C SPARQL Results JSON](https://www.w3.org/TR/sparql11-results-json/).

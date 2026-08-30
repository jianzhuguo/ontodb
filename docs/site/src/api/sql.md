# SQL Reference

OntoDB supports a rich SQL dialect with extensions for vector search and ontology.

## DDL (Data Definition)

### CREATE CLASS

```sql
CREATE CLASS Product (
  id STRING,
  name STRING,
  price FLOAT,
  category STRING,
  in_stock BOOL
)
```

### CREATE INDEX

```sql
CREATE INDEX ON Product (price)
CREATE INDEX ON Product (category, price)
```

### CREATE VECTOR INDEX

```sql
CREATE VECTOR INDEX ON Product (embedding) DIM 128 METRIC cosine
CREATE VECTOR INDEX ON Document (embedding) DIM 768 METRIC l2
```

Supported metrics: `cosine`, `l2`, `inner_product`

### DROP CLASS

```sql
DROP CLASS Product
```

## DML (Data Manipulation)

### INSERT

```sql
INSERT INTO Product (id, name, price) VALUES ("1", "Widget", 9.99)
INSERT INTO Product SET id = "1", name = "Widget", price = 9.99
```

### Batch INSERT

```sql
INSERT INTO Product (id, name, price) VALUES
  ("1", "Widget", 9.99),
  ("2", "Gadget", 19.99),
  ("3", "Gizmo", 29.99)
```

### INSERT SELECT

```sql
INSERT INTO ProductBackup (id, name) SELECT id, name FROM Product WHERE price > 100
```

### UPSERT

```sql
INSERT INTO Product (id, name, price) VALUES ("1", "Widget v2", 14.99)
  ON CONFLICT (id) DO UPDATE SET name = "Widget v2", price = 14.99
```

### UPDATE

```sql
UPDATE Product SET price = 14.99 WHERE id = "1"
UPDATE Product SET price = price * 1.1 WHERE category = "electronics"
```

### DELETE

```sql
DELETE FROM Product WHERE id = "1"
DELETE FROM Product WHERE price < 10
```

## SELECT

### Basic

```sql
SELECT * FROM Product
SELECT name, price FROM Product
SELECT DISTINCT category FROM Product
```

### WHERE

```sql
SELECT * FROM Product WHERE price > 100
SELECT * FROM Product WHERE category = "electronics" AND price < 500
SELECT * FROM Product WHERE name LIKE "%headphone%"
SELECT * FROM Product WHERE price BETWEEN 50 AND 200
SELECT * FROM Product WHERE category IN ("electronics", "sports")
SELECT * FROM Product WHERE description IS NOT NULL
```

### ORDER BY

```sql
SELECT * FROM Product ORDER BY price ASC
SELECT * FROM Product ORDER BY price DESC, name ASC
```

### LIMIT / OFFSET

```sql
SELECT * FROM Product ORDER BY price DESC LIMIT 10
SELECT * FROM Product ORDER BY price DESC LIMIT 10 OFFSET 20
```

### GROUP BY / HAVING

```sql
SELECT category, COUNT(*) as cnt, AVG(price) as avg_price
FROM Product
GROUP BY category
HAVING cnt > 5
ORDER BY avg_price DESC
```

### Aggregate functions

`COUNT(*)`, `SUM(col)`, `AVG(col)`, `MIN(col)`, `MAX(col)`

### JOIN

```sql
SELECT p.name, c.name as category_name
FROM Product p
JOIN Category c ON p.category = c.id
```

### Subqueries

```sql
SELECT * FROM Product WHERE category IN (
  SELECT id FROM Category WHERE active = true
)
```

### CTE (Common Table Expression)

```sql
WITH expensive AS (
  SELECT * FROM Product WHERE price > 100
)
SELECT * FROM expensive WHERE category = "electronics"
```

### Recursive CTE

```sql
WITH RECURSIVE descendants AS (
  SELECT id, name, parent_id FROM Category WHERE id = "root"
  UNION ALL
  SELECT c.id, c.name, c.parent_id FROM Category c
  JOIN descendants d ON c.parent_id = d.id
)
SELECT * FROM descendants
```

### Window Functions

```sql
SELECT name, price,
  ROW_NUMBER() OVER (ORDER BY price DESC) as rank,
  AVG(price) OVER (PARTITION BY category) as category_avg
FROM Product
```

### CASE WHEN

```sql
SELECT name, price,
  CASE
    WHEN price > 100 THEN "premium"
    WHEN price > 50 THEN "mid-range"
    ELSE "budget"
  END as tier
FROM Product
```

## Vector Search

```sql
VECTOR SEARCH ON Product (embedding) QUERY [0.1, 0.2, ...] TOP 10
VECTOR SEARCH ON Product (embedding) QUERY [0.1, 0.2, ...] TOP 10 WHERE category = "electronics"
```

## MATCH (Ontology-aware)

```sql
MATCH ?x ISA Animal WHERE ?x.age > 5
```

## Transactions

```sql
BEGIN
INSERT INTO Product SET id = "1", name = "Widget", price = 9.99
UPDATE Product SET price = 14.99 WHERE id = "1"
COMMIT

BEGIN
DELETE FROM Product WHERE id = "1"
ROLLBACK
```

## Other

```sql
-- Explain query plan
EXPLAIN SELECT * FROM Product WHERE price > 100
EXPLAIN ANALYZE SELECT * FROM Product WHERE price > 100

-- Analyze table statistics
ANALYZE Product

-- Import data
IMPORT INTO Product FROM CSV 'products.csv'
IMPORT INTO Product FROM JSON 'products.json'

-- Backup / Restore
BACKUP TO '/backups/2026-08-08'
RESTORE FROM '/backups/2026-08-08'

-- Flush MemTable to disk
FLUSH
```

## Materialized Views

Pre-computed query results that refresh automatically.

```sql
-- Create materialized view
CREATE MATERIALIZED VIEW product_stats AS
  SELECT category, COUNT(*) as cnt, AVG(price) as avg_price
  FROM Product
  GROUP BY category

-- Refresh materialized view
REFRESH MATERIALIZED VIEW product_stats

-- Query materialized view (same as regular table)
SELECT * FROM product_stats WHERE cnt > 10

-- Drop materialized view
DROP MATERIALIZED VIEW product_stats
```

## System Views

Built-in virtual views for introspection.

```sql
-- List all ontologies
SELECT * FROM system.ontologies

-- List all classes
SELECT * FROM system.classes

-- List all indexes
SELECT * FROM system.indexes

-- List all vector indexes
SELECT * FROM system.vector_indexes

-- Show storage statistics
SELECT * FROM system.storage_stats
```

## OntoQL Extensions

OntoQL extends SQL with semantic capabilities.

### CREATE ONTOLOGY

Define an ontology with classes and properties:

```sql
CREATE ONTOLOGY ECommerce (
  CLASS Product,
  PROPERTY name DOMAIN Product RANGE STRING,
  PROPERTY price DOMAIN Product RANGE FLOAT64,
  PROPERTY category DOMAIN Product RANGE STRING,

  CLASS Order,
  PROPERTY order_id DOMAIN Order RANGE STRING,
  PROPERTY total DOMAIN Order RANGE FLOAT64,

  UNIQUE Product(name)
)
```

### Class Inheritance

```sql
CREATE ONTOLOGY Vehicles (
  CLASS Vehicle,
  PROPERTY brand DOMAIN Vehicle RANGE STRING,

  CLASS Car EXTENDS Vehicle,
  PROPERTY doors DOMAIN Car RANGE INT64,

  CLASS Truck EXTENDS Vehicle,
  PROPERTY payload DOMAIN Truck RANGE FLOAT64
)
```

Querying `Vehicle` automatically returns `Car` and `Truck` instances.

### GRAPH MATCH

```sql
-- Pattern matching on property graph
GRAPH MATCH (a:Person) -[knows]-> (b:Person)
  WHERE a.name = "Alice"
  RETURN b.name, b.age

-- Multi-hop
GRAPH MATCH (a:Person) -[knows*1..3]-> (b:Person)
  RETURN b.name
```

### GRAPH TRAVERSE

```sql
-- BFS traversal
GRAPH TRAVERSE FROM 'Person::1' OUT LABEL 'knows' DEPTH 3

-- With filter
GRAPH TRAVERSE FROM 'Person::1' OUT LABEL 'knows' DEPTH 3 WHERE age > 25
```

### GRAPH SHORTEST PATH

```sql
GRAPH SHORTEST PATH FROM 'Person::1' TO 'Person::5'
GRAPH SHORTEST PATH FROM 'Person::1' TO 'Person::5' LABEL 'knows'
```

### EXPLAIN REASONING

Explain ontology inference:

```sql
EXPLAIN REASONING SELECT * FROM Vehicle
-- Shows: Car instances included via Cax-sco (subclass propagation)
```

### VECTOR INSERT

Insert vector data separately:

```sql
VECTOR INSERT ON Product ("prod_1") embedding [0.1, 0.2, 0.3, ...]
```

### UNIQUE Constraints

```sql
CREATE ONTOLOGY Users (
  CLASS User,
  PROPERTY email DOMAIN User RANGE STRING,
  UNIQUE User(email)
)
```

Duplicate inserts are rejected with an error.

### Property Modifiers

```sql
CREATE ONTOLOGY Schema (
  CLASS Person,
  PROPERTY name DOMAIN Person RANGE STRING REQUIRED,
  PROPERTY tags DOMAIN Person RANGE STRING MULTI_VALUED
)
```

- `REQUIRED` — property must have a value
- `MULTI_VALUED` — property can have multiple values

# Ontology Reasoning

OntoDB's unique feature: **OWL-lite ontology reasoning built into the database engine.**

## What is ontology reasoning?

An ontology defines the structure of your domain:
- **Classes** — types of entities (e.g., `Animal`, `Dog`, `Cat`)
- **Properties** — attributes and relationships (e.g., `name`, `hasOwner`)
- **Restrictions** — constraints (e.g., "every Dog must have exactly one name")
- **Rules** — inference rules (e.g., "Dog is a subclass of Animal")

The reasoner automatically applies these rules during queries.

## Subclass propagation

```sql
-- Define ontology
CREATE ONTOLOGY MyOnto {
  CLASS Animal;
  CLASS Dog SUBCLASS OF Animal;
  CLASS Cat SUBCLASS OF Animal;
}

-- Insert data
INSERT INTO Dog SET id = "1", name = "Rex"

-- Query Animal — automatically includes Dog instances
SELECT * FROM Animal
-- Returns: Rex (even though it's a Dog)
```

## Property inference

```sql
-- Define inverse properties
CREATE ONTOLOGY Family {
  PROPERTY hasParent INVERSE OF hasChild;
}

-- Insert: Alice hasParent Bob
INSERT INTO Family SET id = "1", subject = "Alice", predicate = "hasParent", object = "Bob"

-- Query hasChild — automatically inferred from hasParent
SELECT * FROM Family WHERE predicate = "hasChild"
-- Returns: Bob hasChild Alice
```

## Supported OWL features

| Feature | Example |
|---------|---------|
| SubClassOf | `Dog SUBCLASS OF Animal` |
| EquivalentClass | `Vehicle EQUIVALENT TO Automobile` |
| DisjointClass | `Cat DISJOINT FROM Dog` |
| InverseProperty | `hasParent INVERSE OF hasChild` |
| TransitiveProperty | `hasAncestor TRANSITIVE` |
| SymmetricProperty | `isFriendOf SYMMETRIC` |
| FunctionalProperty | `hasSSN FUNCTIONAL` |
| Cardinality restriction | `Dog REQUIRES name EXACTLY 1` |
| Value restriction | `Dog REQUIRES hasLegs SOME integer` |

## How it works

1. Ontology is stored as a schema in the database
2. When a query runs, the reasoner checks for applicable rules
3. Subclass queries are expanded to include all subclasses
4. Property queries include inferred inverse/transitive relationships
5. Restrictions are validated on write (INSERT/UPDATE)
6. Results are cached to avoid re-reasoning on every query

## Performance

- Reasoning adds ~1-5ms per query (cached after first run)
- Subclass expansion is O(depth) where depth is the class hierarchy depth
- Property inference is O(1) for inverse/symmetric, O(n) for transitive chains

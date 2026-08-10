"""
OntoDB Python SDK
=================

Python client library for OntoDB — the ontology-driven semantic multi-modal database.

Basic usage::

    from ontodb import OntoDB

    db = OntoDB("http://localhost:7912")

    # Execute SQL
    result = db.query("SELECT * FROM users LIMIT 10")
    for row in result:
        print(row)

    # Vector search
    results = db.vector_search("documents", "embedding", [0.1, 0.2, ...], top_k=5)

    # SPARQL query
    results = db.sparql("SELECT ?x WHERE { ?x rdf:type :Person }")

    # Graph traverse
    results = db.graph_traverse("Person::1", direction="out", depth=3)
"""

__version__ = "0.3.0"

from .client import OntoDB
from .exceptions import (
    OntoDBError,
    ConnectionError,
    QueryError,
    AuthenticationError,
    TimeoutError,
)

__all__ = [
    "OntoDB",
    "OntoDBError",
    "ConnectionError",
    "QueryError",
    "AuthenticationError",
    "TimeoutError",
]

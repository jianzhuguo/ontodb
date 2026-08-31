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

SQLAlchemy usage::

    from sqlalchemy import create_engine, text

    engine = create_engine("ontodb://localhost:7912")
    with engine.connect() as conn:
        result = conn.execute(text("SELECT * FROM Product"))
        for row in result:
            print(row)
"""

__version__ = "0.6.2"

from .client import OntoDB
from .exceptions import (
    OntoDBError,
    ConnectionError,
    QueryError,
    AuthenticationError,
    TimeoutError,
)

# Import dialect to auto-register with SQLAlchemy
try:
    from . import dialect
except ImportError:
    pass

# Import AI integrations (optional)
try:
    from . import ai
except ImportError:
    pass

__all__ = [
    "OntoDB",
    "OntoDBError",
    "ConnectionError",
    "QueryError",
    "AuthenticationError",
    "TimeoutError",
]

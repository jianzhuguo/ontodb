"""OntoDB Python SDK - Client library for OntoDB HTTP API."""

__version__ = "0.1.0"

from .client import OntoDBClient
from .exceptions import OntoDBError, ConnectionError, QueryError

__all__ = ["OntoDBClient", "OntoDBError", "ConnectionError", "QueryError"]

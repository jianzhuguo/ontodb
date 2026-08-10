"""OntoDB SDK exceptions."""


class OntoDBError(Exception):
    """Base exception for all OntoDB SDK errors."""
    pass


class ConnectionError(OntoDBError):
    """Failed to connect to OntoDB server."""
    pass


class QueryError(OntoDBError):
    """SQL/SPARQL query execution failed."""
    pass


class AuthenticationError(OntoDBError):
    """Authentication failed (invalid API key)."""
    pass


class TimeoutError(OntoDBError):
    """Request timed out."""
    pass


class SchemaError(OntoDBError):
    """Schema validation error."""
    pass

"""OntoDB Python SDK - Exception classes."""


class OntoDBError(Exception):
    """Base exception for OntoDB SDK errors."""
    pass


class ConnectionError(OntoDBError):
    """Raised when connection to server fails."""
    pass


class QueryError(OntoDBError):
    """Raised when query execution fails."""
    pass


class AuthenticationError(OntoDBError):
    """Raised when authentication fails."""
    pass

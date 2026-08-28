"""
SQLAlchemy Dialect for OntoDB
=============================

Allows using SQLAlchemy ORM with OntoDB database.

Usage::

    from sqlalchemy import create_engine

    # Connect to OntoDB
    engine = create_engine("ontodb://localhost:7912")

    # Or with API key
    engine = create_engine("ontodb://localhost:7912?api_key=your-key")

    # Use with SQLAlchemy ORM
    from sqlalchemy.orm import Session

    with Session(engine) as session:
        result = session.execute(text("SELECT * FROM Product"))
        for row in result:
            print(row)
"""

from sqlalchemy import types as sqltypes
from sqlalchemy.engine import default
from sqlalchemy.sql import compiler

from .client import OntoDB


class OntoDBCompiler(compiler.SQLCompiler):
    """SQL compiler for OntoDB."""

    pass


class OntoDDLCompiler(compiler.DDLCompiler):
    """DDL compiler for OntoDB."""

    pass


class OntoTypeCompiler(compiler.GenericTypeCompiler):
    """Type compiler for OntoDB."""

    pass


class OntoDBIdentifierPreparer(compiler.IdentifierPreparer):
    """Identifier preparer for OntoDB."""

    pass


class OntoDBDialect(default.DefaultDialect):
    """SQLAlchemy dialect for OntoDB."""

    name = "ontodb"
    driver = "ontodb"

    # Capabilities
    supports_alter = False
    supports_pk_autoincrement = False
    supports_default_values = False
    supports_empty_insert = False
    supports_unicode_statements = True
    supports_unicode_binds = True
    returns_unicode_strings = True
    description_encoding = None
    supports_native_boolean = True
    supports_simple_order_by_label = True

    # Use our custom compilers
    statement_compiler = OntoDBCompiler
    ddl_compiler = OntoDDLCompiler
    type_compiler = OntoTypeCompiler
    preparer = OntoDBIdentifierPreparer

    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self._client = None

    @classmethod
    def import_dbapi(cls):
        """Return the DBAPI module."""
        return OntoDBDBAPI()

    def create_connect_args(self, url):
        """Create connection arguments from URL."""
        opts = {
            "base_url": f"http://{url.host}:{url.port or 7912}",
        }
        if url.username:
            opts["api_key"] = url.username
        elif url.query.get("api_key"):
            opts["api_key"] = url.query["api_key"]

        return ([], opts)

    def connect(self, *args, **kwargs):
        """Create a new connection."""
        return OntoDBConnection(**kwargs)

    def do_execute(self, cursor, statement, parameters, context=None):
        """Execute a statement."""
        cursor.execute(statement, parameters)

    def do_executemany(self, cursor, statement, parameters, context=None):
        """Execute a statement with multiple parameter sets."""
        for params in parameters:
            cursor.execute(statement, params)

    def has_table(self, connection, table_name, schema=None, **kwargs):
        """Check if a table exists."""
        try:
            result = connection.execute(f"SELECT * FROM {table_name} LIMIT 0")
            return True
        except Exception:
            return False

    def get_table_names(self, connection=None, schema=None, **kwargs):
        """Get list of table names."""
        try:
            result = connection.execute("SELECT * FROM __ontology__")
            # Parse table names from ontology results
            tables = []
            if result and hasattr(result, 'fetchall'):
                for row in result.fetchall():
                    if isinstance(row, dict) and 'name' in row:
                        tables.append(row['name'])
                    elif isinstance(row, (list, tuple)) and len(row) > 0:
                        tables.append(str(row[0]))
            return tables
        except Exception:
            return []

    def get_columns(self, connection, table_name, schema=None, **kwargs):
        """Get column information for a table."""
        # OntoDB is schema-on-read, so we return empty list
        # Users can query the schema API for more details
        return []

    def get_pk_constraint(self, connection, table_name, schema=None, **kwargs):
        """Get primary key constraint."""
        return {"constrained_columns": [], "name": None}

    def get_foreign_keys(self, connection, table_name, schema=None, **kwargs):
        """Get foreign keys."""
        return []

    def get_indexes(self, connection, table_name, schema=None, **kwargs):
        """Get indexes."""
        return []

    def get_schema_names(self, connection=None, **kwargs):
        """Get schema names."""
        return ["default"]


class OntoDBDBAPI:
    """Minimal DBAPI 2.0 interface for OntoDB."""

    apilevel = "2.0"
    threadsafety = 1
    paramstyle = "qmark"

    class Error(Exception):
        pass

    class DatabaseError(Error):
        pass

    class InterfaceError(Error):
        pass

    class OperationalError(Error):
        pass

    class ProgrammingError(Error):
        pass

    @staticmethod
    def connect(*args, **kwargs):
        return OntoDBConnection(**kwargs)


class OntoDBConnection:
    """DBAPI connection wrapper for OntoDB."""

    def __init__(self, base_url="http://localhost:7912", api_key=None, **kwargs):
        self._client = OntoDB(base_url=base_url, api_key=api_key)
        self._closed = False

    def cursor(self):
        """Create a new cursor."""
        if self._closed:
            raise OntoDBDBAPI.InterfaceError("Connection is closed")
        return OntoDBCursor(self._client)

    def close(self):
        """Close the connection."""
        self._closed = True

    def commit(self):
        """Commit (no-op for OntoDB's auto-commit mode)."""
        pass

    def rollback(self):
        """Rollback (no-op for OntoDB's auto-commit mode)."""
        pass

    def __enter__(self):
        return self

    def __exit__(self, *args):
        self.close()


class OntoDBCursor:
    """DBAPI cursor wrapper for OntoDB."""

    def __init__(self, client):
        self._client = client
        self._results = None
        self._description = None
        self._rowcount = -1
        self._closed = False

    @property
    def description(self):
        """Return cursor description (column info)."""
        return self._description

    @property
    def rowcount(self):
        """Return number of rows affected."""
        return self._rowcount

    def execute(self, operation, parameters=None):
        """Execute a query."""
        if self._closed:
            raise OntoDBDBAPI.InterfaceError("Cursor is closed")

        # Substitute parameters if provided
        if parameters:
            # Simple qmark parameter substitution
            query = operation
            for param in parameters:
                query = query.replace("?", self._format_param(param), 1)
        else:
            query = operation

        try:
            result = self._client.query(query)
            self._results = result
            if result:
                self._rowcount = len(result) if isinstance(result, list) else -1
                # Build description from first row
                if isinstance(result, list) and len(result) > 0 and isinstance(result[0], dict):
                    self._description = [
                        (col, None, None, None, None, None, None)
                        for col in result[0].keys()
                    ]
                else:
                    self._description = None
            else:
                self._results = []
                self._rowcount = 0
        except Exception as e:
            raise OntoDBDBAPI.DatabaseError(str(e))

    def executemany(self, operation, seq_of_parameters):
        """Execute a query with multiple parameter sets."""
        for params in seq_of_parameters:
            self.execute(operation, params)

    def fetchone(self):
        """Fetch one row."""
        if self._results and len(self._results) > 0:
            row = self._results.pop(0)
            if isinstance(row, dict):
                return list(row.values())
            return row
        return None

    def fetchmany(self, size=None):
        """Fetch multiple rows."""
        if size is None:
            size = self.arraysize
        rows = []
        for _ in range(min(size, len(self._results or []))):
            row = self.fetchone()
            if row is None:
                break
            rows.append(row)
        return rows

    def fetchall(self):
        """Fetch all rows."""
        rows = []
        while True:
            row = self.fetchone()
            if row is None:
                break
            rows.append(row)
        return rows

    def close(self):
        """Close the cursor."""
        self._closed = True
        self._results = None
        self._description = None

    def _format_param(self, param):
        """Format a parameter for SQL."""
        if param is None:
            return "NULL"
        elif isinstance(param, bool):
            return "true" if param else "false"
        elif isinstance(param, (int, float)):
            return str(param)
        elif isinstance(param, str):
            return f"'{param.replace(chr(39), chr(39)+chr(39))}'"
        else:
            return f"'{param}'"

    @property
    def arraysize(self):
        """Default array size for fetchmany."""
        return 10

    @arraysize.setter
    def arraysize(self, value):
        pass


# Register the dialect with SQLAlchemy
def register_dialect():
    """Register the OntoDB dialect with SQLAlchemy."""
    try:
        from sqlalchemy.dialects import registry
        registry.register("ontodb", "ontodb.dialect", "OntoDBDialect")
    except ImportError:
        pass


# Auto-register when module is imported
register_dialect()

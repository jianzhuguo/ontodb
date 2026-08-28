"""Tests for SQLAlchemy dialect."""

import pytest


class TestDialectImport:
    """Test dialect import and registration."""

    def test_dialect_module_exists(self):
        """Test that dialect module can be imported."""
        from ontodb import dialect
        assert hasattr(dialect, 'OntoDBDialect')

    def test_dialect_class_attributes(self):
        """Test dialect class has required attributes."""
        from ontodb.dialect import OntoDBDialect
        assert OntoDBDialect.name == "ontodb"
        assert OntoDBDialect.driver == "ontodb"

    def test_dbapi(self):
        """Test DBAPI is returned."""
        from ontodb.dialect import OntoDBDialect
        dialect_inst = OntoDBDialect()
        dbapi = dialect_inst.import_dbapi()
        assert dbapi.apilevel == "2.0"
        assert dbapi.paramstyle == "qmark"


class TestDBAPIConnection:
    """Test DBAPI connection."""

    def test_connection_create(self):
        """Test connection creation."""
        from ontodb.dialect import OntoDBConnection
        conn = OntoDBConnection(base_url="http://localhost:7912")
        assert conn is not None
        conn.close()

    def test_connection_context_manager(self):
        """Test connection as context manager."""
        from ontodb.dialect import OntoDBConnection
        with OntoDBConnection(base_url="http://localhost:7912") as conn:
            assert conn is not None

    def test_cursor_create(self):
        """Test cursor creation."""
        from ontodb.dialect import OntoDBConnection
        conn = OntoDBConnection(base_url="http://localhost:7912")
        cursor = conn.cursor()
        assert cursor is not None
        cursor.close()
        conn.close()

    def test_closed_connection_raises(self):
        """Test that closed connection raises error."""
        from ontodb.dialect import OntoDBConnection, OntoDBDBAPI
        conn = OntoDBConnection(base_url="http://localhost:7912")
        conn.close()
        with pytest.raises(OntoDBDBAPI.InterfaceError):
            conn.cursor()


class TestDBAPICursor:
    """Test DBAPI cursor."""

    def test_cursor_description_initial(self):
        """Test cursor description is None initially."""
        from ontodb.dialect import OntoDBConnection
        conn = OntoDBConnection(base_url="http://localhost:7912")
        cursor = conn.cursor()
        assert cursor.description is None
        cursor.close()
        conn.close()

    def test_cursor_rowcount_initial(self):
        """Test cursor rowcount is -1 initially."""
        from ontodb.dialect import OntoDBConnection
        conn = OntoDBConnection(base_url="http://localhost:7912")
        cursor = conn.cursor()
        assert cursor.rowcount == -1
        cursor.close()
        conn.close()


class TestDialectURL:
    """Test dialect URL parsing."""

    def test_create_connect_args(self):
        """Test connection args creation from URL."""
        from ontodb.dialect import OntoDBDialect
        from sqlalchemy.engine import make_url

        dialect = OntoDBDialect()
        url = make_url("ontodb://localhost:7912")
        args, kwargs = dialect.create_connect_args(url)
        assert kwargs["base_url"] == "http://localhost:7912"

    def test_create_connect_args_with_api_key(self):
        """Test connection args with API key."""
        from ontodb.dialect import OntoDBDialect
        from sqlalchemy.engine import make_url

        dialect = OntoDBDialect()
        url = make_url("ontodb://mykey@localhost:7912")
        args, kwargs = dialect.create_connect_args(url)
        assert kwargs["api_key"] == "mykey"

    def test_dialect_capabilities(self):
        """Test dialect capabilities."""
        from ontodb.dialect import OntoDBDialect
        dialect = OntoDBDialect()
        assert dialect.supports_unicode_statements is True
        assert dialect.supports_native_boolean is True
        assert dialect.supports_alter is False


class TestSQLAlchemyIntegration:
    """Test SQLAlchemy integration (requires sqlalchemy installed)."""

    def test_engine_creation(self):
        """Test engine can be created."""
        try:
            from sqlalchemy import create_engine
            engine = create_engine("ontodb://localhost:7912")
            assert engine is not None
        except ImportError:
            pytest.skip("sqlalchemy not installed")

    def test_engine_url(self):
        """Test engine URL is parsed correctly."""
        try:
            from sqlalchemy import create_engine
            engine = create_engine("ontodb://localhost:7912")
            assert "ontodb" in str(engine.url)
        except ImportError:
            pytest.skip("sqlalchemy not installed")

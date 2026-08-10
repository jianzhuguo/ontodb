"""OntoDB SDK tests."""

import pytest
import responses

from ontodb import OntoDB, OntoDBError, QueryError, AuthenticationError, ConnectionError


BASE_URL = "http://localhost:7912"


@pytest.fixture
def db():
    return OntoDB(BASE_URL, max_retries=0)


@pytest.fixture
def db_auth():
    return OntoDB(BASE_URL, api_key="test-key", max_retries=0)


class TestConnection:
    def test_init(self, db):
        assert db.base_url == BASE_URL
        assert db.timeout == 30.0

    def test_repr(self, db):
        assert repr(db) == f"OntoDB('{BASE_URL}')"

    def test_context_manager(self):
        with OntoDB(BASE_URL) as db:
            assert db is not None

    @responses.activate
    def test_health(self, db):
        responses.add(responses.GET, f"{BASE_URL}/api/health", json={"status": "ok"}, status=200)
        result = db.health()
        assert result["status"] == "ok"

    @responses.activate
    def test_is_ready(self, db):
        responses.add(responses.GET, f"{BASE_URL}/api/health", json={"status": "ok"}, status=200)
        assert db.is_ready() is True

    @responses.activate
    def test_is_ready_failure(self, db):
        responses.add(responses.GET, f"{BASE_URL}/api/health", json={"error": "down"}, status=500)
        assert db.is_ready() is False


class TestAuthentication:
    @responses.activate
    def test_api_key_header(self, db_auth):
        responses.add(responses.GET, f"{BASE_URL}/api/health", json={"status": "ok"}, status=200)
        db_auth.health()
        assert responses.calls[0].request.headers["Authorization"] == "Bearer test-key"

    @responses.activate
    def test_auth_error(self, db):
        responses.add(responses.GET, f"{BASE_URL}/api/health", json={"error": "unauthorized"}, status=401)
        with pytest.raises(AuthenticationError):
            db.health()


class TestQuery:
    @responses.activate
    def test_select(self, db):
        responses.add(
            responses.POST, f"{BASE_URL}/api/query",
            json={"data": [{"name": "Alice", "age": 30}]},
            status=200,
        )
        rows = db.query("SELECT * FROM users")
        assert len(rows) == 1
        assert rows[0]["name"] == "Alice"

    @responses.activate
    def test_insert(self, db):
        responses.add(
            responses.POST, f"{BASE_URL}/api/query",
            json={"data": [], "rows_affected": 1},
            status=200,
        )
        result = db.execute("INSERT INTO users (name) VALUES ('Bob')")
        assert result["rows_affected"] == 1

    @responses.activate
    def test_query_error(self, db):
        responses.add(
            responses.POST, f"{BASE_URL}/api/query",
            json={"error": "table not found"},
            status=200,
        )
        with pytest.raises(QueryError, match="table not found"):
            db.query("SELECT * FROM nonexistent")

    @responses.activate
    def test_query_many(self, db):
        responses.add(
            responses.POST, f"{BASE_URL}/api/query",
            json={"data": [{"count": 10}]},
            status=200,
        )
        responses.add(
            responses.POST, f"{BASE_URL}/api/query",
            json={"data": [{"count": 5}]},
            status=200,
        )
        results = db.query_many(["SELECT COUNT(*) FROM users", "SELECT COUNT(*) FROM orders"])
        assert len(results) == 2


class TestVectorSearch:
    @responses.activate
    def test_vector_search(self, db):
        responses.add(
            responses.POST, f"{BASE_URL}/api/vector/search",
            json={"data": [{"title": "doc1", "_score": 0.95}]},
            status=200,
        )
        results = db.vector_search("documents", "embedding", [0.1, 0.2], top_k=1)
        assert len(results) == 1
        assert results[0]["_score"] == 0.95

    @responses.activate
    def test_hybrid_search(self, db):
        responses.add(
            responses.POST, f"{BASE_URL}/api/hybrid/query",
            json={"data": [{"title": "doc2"}]},
            status=200,
        )
        results = db.hybrid_search("documents", "embedding", [0.1, 0.2], "category = 'tech'")
        assert len(results) == 1


class TestSPARQL:
    @responses.activate
    def test_sparql(self, db):
        responses.add(
            responses.POST, f"{BASE_URL}/api/sparql",
            json={"data": [{"name": "Alice"}]},
            status=200,
        )
        results = db.sparql("SELECT ?name WHERE { ?p ex:name ?name }")
        assert len(results) == 1


class TestGraph:
    @responses.activate
    def test_traverse(self, db):
        responses.add(
            responses.POST, f"{BASE_URL}/api/graph/traverse",
            json={"data": {"vertices": [{"id": "P1"}], "edges": []}},
            status=200,
        )
        result = db.graph_traverse("Person::1", direction="out", depth=2)
        assert "vertices" in result

    @responses.activate
    def test_shortest_path(self, db):
        responses.add(
            responses.POST, f"{BASE_URL}/api/graph/shortest-path",
            json={"data": {"path": ["P1", "P2", "P3"]}},
            status=200,
        )
        path = db.graph_shortest_path("Person::1", "Person::3")
        assert len(path) == 3


class TestBackup:
    @responses.activate
    def test_backup(self, db):
        responses.add(
            responses.POST, f"{BASE_URL}/api/backup",
            json={"path": "/backups/test.ontodb"},
            status=200,
        )
        result = db.backup("/backups/test.ontodb")
        assert result["path"] == "/backups/test.ontodb"


class TestInsertMany:
    @responses.activate
    def test_insert_many(self, db):
        responses.add(
            responses.POST, f"{BASE_URL}/api/query",
            json={"data": [], "rows_affected": 2},
            status=200,
        )
        result = db.insert_many("users", [
            {"name": "Alice", "age": 30},
            {"name": "Bob", "age": 25},
        ])
        assert result["rows_affected"] == 2

    def test_insert_many_empty(self, db):
        result = db.insert_many("users", [])
        assert result == {}


class TestErrorHandling:
    def test_connection_error(self):
        db = OntoDB("http://127.0.0.1:1", max_retries=0)
        with pytest.raises((ConnectionError, OntoDBError)):
            db.health()

    @responses.activate
    def test_rate_limit(self, db):
        responses.add(responses.GET, f"{BASE_URL}/api/health", json={"error": "rate limited"}, status=429)
        from ontodb import OntoDBError
        with pytest.raises(OntoDBError, match="Rate limit"):
            db.health()

    @responses.activate
    def test_server_error(self, db):
        responses.add(responses.GET, f"{BASE_URL}/api/health", body="Internal Server Error", status=500)
        with pytest.raises(QueryError, match="500"):
            db.health()

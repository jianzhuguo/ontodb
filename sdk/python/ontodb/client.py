"""OntoDB Python SDK — main client."""

import time
from typing import Any, Dict, List, Optional, Union

import requests

from .exceptions import (
    AuthenticationError,
    ConnectionError,
    OntoDBError,
    QueryError,
    TimeoutError,
)
from .models import QueryResult, VectorSearchResult, GraphResult, SchemaInfo


class OntoDB:
    """OntoDB client.

    Args:
        base_url: Server URL (e.g., "http://localhost:7912")
        api_key: API key for authentication
        timeout: Default request timeout in seconds
        max_retries: Maximum number of retries on failure

    Example::

        db = OntoDB("http://localhost:7912", api_key="your-key")
        result = db.query("SELECT * FROM users")
    """

    def __init__(
        self,
        base_url: str = "http://localhost:7912",
        api_key: Optional[str] = None,
        timeout: float = 30.0,
        max_retries: int = 3,
    ):
        self.base_url = base_url.rstrip("/")
        self.timeout = timeout
        self.max_retries = max_retries
        self._session = requests.Session()
        if api_key:
            self._session.headers["Authorization"] = f"Bearer {api_key}"
        self._session.headers["Content-Type"] = "application/json"

    def _request(
        self,
        method: str,
        path: str,
        json: Optional[Dict] = None,
        timeout: Optional[float] = None,
    ) -> Dict[str, Any]:
        """Send HTTP request with retry logic."""
        url = f"{self.base_url}{path}"
        last_error = None

        for attempt in range(self.max_retries + 1):
            try:
                resp = self._session.request(
                    method,
                    url,
                    json=json,
                    timeout=timeout or self.timeout,
                )

                if resp.status_code == 401:
                    raise AuthenticationError("Invalid API key")
                if resp.status_code == 429:
                    raise OntoDBError("Rate limit exceeded")
                if resp.status_code >= 400:
                    try:
                        body = resp.json()
                        msg = body.get("error", resp.text)
                    except Exception:
                        msg = resp.text
                    raise QueryError(f"HTTP {resp.status_code}: {msg}")

                return resp.json()

            except requests.exceptions.ConnectionError as e:
                last_error = ConnectionError(f"Cannot connect to {self.base_url}: {e}")
                if attempt < self.max_retries:
                    time.sleep(0.5 * (attempt + 1))
                    continue
                raise last_error

            except requests.exceptions.Timeout:
                last_error = TimeoutError(f"Request timed out after {timeout or self.timeout}s")
                if attempt < self.max_retries:
                    continue
                raise last_error

            except (QueryError, AuthenticationError):
                raise

            except OntoDBError:
                raise

            except Exception as e:
                raise OntoDBError(f"Unexpected error: {e}")

        raise last_error or OntoDBError("Max retries exceeded")

    # ──────────────────────────────────────────────
    # SQL Queries
    # ──────────────────────────────────────────────

    def query(self, sql: str, timeout: Optional[float] = None) -> List[Dict[str, Any]]:
        """Execute a SQL query and return results.

        Args:
            sql: SQL query string
            timeout: Request timeout in seconds

        Returns:
            List of row dictionaries

        Example::

            rows = db.query("SELECT * FROM users WHERE age > 25")
            for row in rows:
                print(row["name"], row["age"])
        """
        result = self._request("POST", "/api/query", json={"query": sql}, timeout=timeout)
        if result.get("error"):
            raise QueryError(result["error"])
        return result.get("data", [])

    def execute(self, sql: str, timeout: Optional[float] = None) -> Dict[str, Any]:
        """Execute a SQL statement (INSERT/UPDATE/DELETE/DDL).

        Args:
            sql: SQL statement
            timeout: Request timeout in seconds

        Returns:
            Response dict with status info

        Example::

            db.execute("INSERT INTO users (name, age) VALUES ('Alice', 30)")
        """
        result = self._request("POST", "/api/query", json={"query": sql}, timeout=timeout)
        if result.get("error"):
            raise QueryError(result["error"])
        return result

    def query_many(self, sqls: List[str], timeout: Optional[float] = None) -> List[List[Dict]]:
        """Execute multiple SQL queries.

        Args:
            sqls: List of SQL query strings
            timeout: Per-query timeout

        Returns:
            List of result sets
        """
        results = []
        for sql in sqls:
            results.append(self.query(sql, timeout=timeout))
        return results

    # ──────────────────────────────────────────────
    # Vector Search
    # ──────────────────────────────────────────────

    def vector_search(
        self,
        table: str,
        column: str,
        vector: List[float],
        top_k: int = 10,
        filter_expr: Optional[str] = None,
        timeout: Optional[float] = None,
    ) -> List[Dict[str, Any]]:
        """Search for similar vectors.

        Args:
            table: Table/class name
            column: Vector column name
            query_vector: Query vector
            top_k: Number of results to return
            filter_expr: Optional SQL WHERE filter

        Returns:
            List of matching rows with similarity scores

        Example::

            results = db.vector_search(
                "documents", "embedding",
                [0.1, 0.2, 0.3, ...],
                top_k=5
            )
            for r in results:
                print(r["title"], r.get("_score"))
        """
        body = {
            "class": table,
            "column": column,
            "query_vector": vector,
            "top_k": top_k,
        }
        if filter_expr:
            body["filter"] = filter_expr

        result = self._request("POST", "/api/vector/search", json=body, timeout=timeout)
        if result.get("error"):
            raise QueryError(result["error"])
        return result.get("data", [])

    def vector_search_multi(
        self,
        table: str,
        searches: List[Dict[str, Any]],
        top_k: int = 10,
        timeout: Optional[float] = None,
    ) -> List[Dict[str, Any]]:
        """Multi-vector search: search multiple vector columns and combine results.

        Args:
            table: Table/class name
            searches: List of search specs, each with 'column', 'query_vector', 'weight'
            top_k: Number of results to return

        Returns:
            List of matching rows with combined scores

        Example::

            results = db.vector_search_multi("Product", [
                {"column": "title_embedding", "query_vector": [...], "weight": 0.7},
                {"column": "image_embedding", "query_vector": [...], "weight": 0.3},
            ], top_k=10)
        """
        body = {
            "class": table,
            "searches": searches,
            "top_k": top_k,
        }
        result = self._request("POST", "/api/vector/search-multi", json=body, timeout=timeout)
        if result.get("error"):
            raise QueryError(result["error"])
        return result.get("data", [])

    def vector_cluster(
        self,
        table: str,
        column: str,
        k: int,
        timeout: Optional[float] = None,
    ) -> Dict[str, Any]:
        """Cluster vectors using K-Means.

        Args:
            table: Table/class name
            column: Vector column name
            k: Number of clusters

        Returns:
            Clustering result with centroids and assignments

        Example::

            result = db.vector_cluster("Product", "embedding", k=5)
            for cluster in result["clusters"]:
                print(f"Cluster {cluster['id']}: {cluster['member_count']} members")
        """
        body = {
            "class": table,
            "column": column,
            "k": k,
        }
        result = self._request("POST", "/api/vector/cluster", json=body, timeout=timeout)
        if result.get("error"):
            raise QueryError(result["error"])
        return result.get("data", {})

    def hybrid_search(
        self,
        table: str,
        vector_column: str,
        vector: List[float],
        sql_filter: str = "",
        top_k: int = 10,
        timeout: Optional[float] = None,
    ) -> List[Dict[str, Any]]:
        """Hybrid SQL + vector search.

        Args:
            table: Table name
            vector_column: Vector column name
            vector: Query vector
            sql_filter: SQL WHERE clause
            top_k: Number of results

        Returns:
            List of matching rows
        """
        body = {
            "class": table,
            "vector_column": vector_column,
            "query_vector": vector,
            "top_k": top_k,
            "filter": sql_filter,
        }
        result = self._request("POST", "/api/hybrid/query", json=body, timeout=timeout)
        if result.get("error"):
            raise QueryError(result["error"])
        return result.get("data", [])

    # ──────────────────────────────────────────────
    # SPARQL
    # ──────────────────────────────────────────────

    def sparql(self, query: str, timeout: Optional[float] = None) -> List[Dict[str, Any]]:
        """Execute a SPARQL query.

        Args:
            query: SPARQL query string
            timeout: Request timeout

        Returns:
            List of result bindings

        Example::

            results = db.sparql('''
                PREFIX ex: <http://example.org/>
                SELECT ?name WHERE { ?p ex:name ?name }
            ''')
        """
        result = self._request("POST", "/api/sparql", json={"query": query}, timeout=timeout)
        if result.get("error"):
            raise QueryError(result["error"])
        return result.get("data", [])

    # ──────────────────────────────────────────────
    # Graph Operations
    # ──────────────────────────────────────────────

    def graph_traverse(
        self,
        start_id: str,
        direction: str = "out",
        edge_label: Optional[str] = None,
        depth: int = 3,
        algorithm: str = "bfs",
        timeout: Optional[float] = None,
    ) -> Dict[str, Any]:
        """Traverse the graph from a starting vertex.

        Args:
            start_id: Starting vertex ID
            direction: "in", "out", or "both"
            edge_label: Filter by edge label
            depth: Maximum traversal depth
            algorithm: "bfs" or "dfs"

        Returns:
            Traversal result with vertices and edges

        Example::

            result = db.graph_traverse("Person::1", direction="out", depth=2)
            for vertex in result.get("vertices", []):
                print(vertex)
        """
        body = {
            "start": start_id,
            "direction": direction,
            "max_depth": depth,
            "algorithm": algorithm,
        }
        if edge_label:
            body["edge_label"] = edge_label

        result = self._request("POST", "/api/graph/traverse", json=body, timeout=timeout)
        if result.get("error"):
            raise QueryError(result["error"])
        return result.get("data", {})

    def graph_shortest_path(
        self,
        from_id: str,
        to_id: str,
        timeout: Optional[float] = None,
    ) -> List[str]:
        """Find shortest path between two vertices.

        Args:
            from_id: Source vertex ID
            to_id: Target vertex ID

        Returns:
            List of vertex IDs on the path
        """
        body = {"from": from_id, "to": to_id}
        result = self._request("POST", "/api/graph/shortest-path", json=body, timeout=timeout)
        if result.get("error"):
            raise QueryError(result["error"])
        return result.get("data", {}).get("path", [])

    # ──────────────────────────────────────────────
    # Schema
    # ──────────────────────────────────────────────

    def schema(self, timeout: Optional[float] = None) -> Dict[str, Any]:
        """Get database schema information.

        Returns:
            Schema dict with tables, columns, indexes
        """
        result = self._request("GET", "/api/schema", timeout=timeout)
        return result.get("data", {})

    # ──────────────────────────────────────────────
    # Health & Metrics
    # ──────────────────────────────────────────────

    def health(self, timeout: Optional[float] = 5.0) -> Dict[str, Any]:
        """Check server health.

        Returns:
            Health status dict
        """
        return self._request("GET", "/api/health", timeout=timeout)

    def metrics(self, timeout: Optional[float] = 5.0) -> Dict[str, Any]:
        """Get server metrics.

        Returns:
            Metrics dict with QPS, latency, storage stats
        """
        return self._request("GET", "/api/metrics", timeout=timeout)

    def is_ready(self) -> bool:
        """Check if server is ready to accept requests."""
        try:
            resp = self.health(timeout=2.0)
            return resp.get("status") == "ok"
        except Exception:
            return False

    # ──────────────────────────────────────────────
    # Backup
    # ──────────────────────────────────────────────

    def backup(self, path: str, timeout: Optional[float] = None) -> Dict[str, Any]:
        """Create a full backup.

        Args:
            path: Backup file path on server

        Returns:
            Backup result with file path and size
        """
        result = self._request("POST", "/api/backup", json={"path": path}, timeout=timeout)
        if result.get("error"):
            raise QueryError(result["error"])
        return result

    def restore(self, path: str, timeout: Optional[float] = None) -> Dict[str, Any]:
        """Restore from backup.

        Args:
            path: Backup file path on server
        """
        result = self._request("POST", "/api/restore", json={"path": path}, timeout=timeout)
        if result.get("error"):
            raise QueryError(result["error"])
        return result

    # ──────────────────────────────────────────────
    # Batch Operations
    # ──────────────────────────────────────────────

    def insert_many(
        self,
        table: str,
        rows: List[Dict[str, Any]],
        timeout: Optional[float] = None,
    ) -> Dict[str, Any]:
        """Insert multiple rows using BATCH INSERT.

        Args:
            table: Table name
            rows: List of row dicts

        Returns:
            Insert result

        Example::

            db.insert_many("users", [
                {"name": "Alice", "age": 30},
                {"name": "Bob", "age": 25},
            ])
        """
        if not rows:
            return {}

        columns = list(rows[0].keys())
        cols_str = ", ".join(columns)

        values = []
        for row in rows:
            vals = []
            for col in columns:
                v = row.get(col)
                if v is None:
                    vals.append("NULL")
                elif isinstance(v, str):
                    vals.append(f"'{v.replace(chr(39), chr(39)*2)}'")
                elif isinstance(v, bool):
                    vals.append("TRUE" if v else "FALSE")
                else:
                    vals.append(str(v))
            values.append(f"({', '.join(vals)})")

        sql = f"BATCH INSERT INTO {table} ({cols_str}) VALUES {', '.join(values)}"
        return self.execute(sql, timeout=timeout)

    # ──────────────────────────────────────────────
    # Context Manager
    # ──────────────────────────────────────────────

    def close(self):
        """Close the HTTP session."""
        self._session.close()

    def __enter__(self):
        return self

    def __exit__(self, *args):
        self.close()

    def __repr__(self):
        return f"OntoDB('{self.base_url}')"

"""OntoDB Python SDK - Client library for OntoDB HTTP API."""

import requests
from typing import Any, Dict, List, Optional, Union
from .exceptions import OntoDBError, ConnectionError, QueryError, AuthenticationError


class OntoDBClient:
    """Client for interacting with OntoDB HTTP API.
    
    Example:
        ```python
        from ontodb import OntoDBClient
        
        client = OntoDBClient("http://localhost:8080")
        
        # Execute SQL query
        result = client.query("SELECT * FROM Product WHERE price > 100")
        
        # Execute SPARQL query
        result = client.sparql("SELECT ?name WHERE { ?p <name> ?name }")
        
        # Vector search
        results = client.vector_search("Product", "embedding", [0.1, 0.2, 0.3], top_k=10)
        ```
    """
    
    def __init__(
        self,
        base_url: str = "http://localhost:8080",
        api_key: Optional[str] = None,
        timeout: int = 30,
    ):
        """Initialize the OntoDB client.
        
        Args:
            base_url: Base URL of the OntoDB server
            api_key: Optional API key for authentication
            timeout: Request timeout in seconds
        """
        self.base_url = base_url.rstrip("/")
        self.timeout = timeout
        self.session = requests.Session()
        
        if api_key:
            self.session.headers["Authorization"] = f"Bearer {api_key}"
    
    def _request(
        self,
        method: str,
        path: str,
        json: Optional[Dict] = None,
        **kwargs,
    ) -> Dict[str, Any]:
        """Make an HTTP request to the OntoDB server.
        
        Args:
            method: HTTP method (GET, POST, etc.)
            path: API path
            json: JSON request body
            **kwargs: Additional request arguments
            
        Returns:
            Response JSON data
            
        Raises:
            ConnectionError: If connection fails
            QueryError: If query execution fails
            AuthenticationError: If authentication fails
        """
        url = f"{self.base_url}{path}"
        
        try:
            response = self.session.request(
                method,
                url,
                json=json,
                timeout=self.timeout,
                **kwargs,
            )
        except requests.exceptions.ConnectionError:
            raise ConnectionError(f"Failed to connect to {self.base_url}")
        except requests.exceptions.Timeout:
            raise ConnectionError(f"Request timed out after {self.timeout}s")
        
        if response.status_code == 401:
            raise AuthenticationError("Invalid or missing API key")
        
        if response.status_code == 429:
            raise OntoDBError("Rate limit exceeded")
        
        data = response.json()
        
        if not data.get("success", True):
            raise QueryError(data.get("error", "Unknown error"))
        
        return data
    
    def health(self) -> Dict[str, Any]:
        """Check server health.
        
        Returns:
            Health status dictionary
        """
        return self._request("GET", "/api/health")
    
    def ready(self) -> bool:
        """Check if server is ready to accept traffic.
        
        Returns:
            True if ready
        """
        try:
            result = self._request("GET", "/api/health/ready")
            return result.get("status") == "ready"
        except OntoDBError:
            return False
    
    def live(self) -> bool:
        """Check if server is alive.
        
        Returns:
            True if alive
        """
        try:
            result = self._request("GET", "/api/health/live")
            return result.get("status") == "alive"
        except OntoDBError:
            return False
    
    def metrics(self) -> Dict[str, Any]:
        """Get server metrics in JSON format.
        
        Returns:
            Metrics dictionary
        """
        return self._request("GET", "/api/metrics")
    
    def schema(self) -> Dict[str, Any]:
        """Get database schema information.
        
        Returns:
            Schema dictionary with classes, indexes, etc.
        """
        return self._request("GET", "/api/schema")
    
    def query(
        self,
        sql: str,
        pretty: bool = False,
    ) -> Union[List[Dict[str, Any]], Dict[str, Any]]:
        """Execute a SQL query.
        
        Args:
            sql: SQL query string
            pretty: Pretty-print JSON results
            
        Returns:
            Query results as list of dictionaries, or status message
            
        Example:
            ```python
            # SELECT query
            rows = client.query("SELECT * FROM Product WHERE price > 100")
            
            # INSERT query
            result = client.query("INSERT INTO Product (name, price) VALUES ('iPhone', 999)")
            ```
        """
        data = self._request("POST", "/api/query", json={
            "query": sql,
            "pretty": pretty,
        })
        
        if "data" in data:
            return data["data"]
        return data
    
    def sparql(self, query: str) -> Dict[str, Any]:
        """Execute a SPARQL query.
        
        Args:
            query: SPARQL query string
            
        Returns:
            SPARQL results in W3C JSON format
            
        Example:
            ```python
            result = client.sparql("""
                PREFIX ex: <http://example.org/>
                SELECT ?name WHERE {
                    ?p a ex:Product .
                    ?p ex:name ?name
                }
            """)
            ```
        """
        return self._request("POST", "/sparql", json={
            "query": query,
        })
    
    def vector_search(
        self,
        class_name: str,
        column: str,
        query_vector: List[float],
        top_k: int = 10,
        filter_expr: Optional[str] = None,
    ) -> List[Dict[str, Any]]:
        """Perform vector similarity search.
        
        Args:
            class_name: Target class name
            column: Vector column name
            query_vector: Query vector for similarity search
            top_k: Number of top results to return
            filter_expr: Optional SQL WHERE clause for hybrid filtering
            
        Returns:
            List of search results with similarity scores
            
        Example:
            ```python
            results = client.vector_search(
                "Product",
                "embedding",
                [0.1, 0.2, 0.3, 0.4],
                top_k=5,
                filter_expr="price > 100"
            )
            ```
        """
        payload = {
            "class": class_name,
            "column": column,
            "query_vector": query_vector,
            "top_k": top_k,
        }
        if filter_expr:
            payload["filter"] = filter_expr
        
        data = self._request("POST", "/api/vector/search", json=payload)
        return data.get("data", [])
    
    def hybrid_query(
        self,
        sql_filter: str,
        vector_column: str,
        query_vector: List[float],
        top_k: int = 10,
        class_name: Optional[str] = None,
    ) -> List[Dict[str, Any]]:
        """Execute a hybrid SQL + vector search query.
        
        Args:
            sql_filter: SQL query for filtering
            vector_column: Vector column for similarity ranking
            query_vector: Query vector for similarity search
            top_k: Number of top results to return
            class_name: Optional target class name
            
        Returns:
            List of search results
        """
        payload = {
            "sql_filter": sql_filter,
            "vector_column": vector_column,
            "query_vector": query_vector,
            "top_k": top_k,
        }
        if class_name:
            payload["class"] = class_name
        
        data = self._request("POST", "/api/hybrid/query", json=payload)
        return data.get("data", [])

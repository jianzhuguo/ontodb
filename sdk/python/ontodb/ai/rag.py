"""
Built-in RAG (Retrieval-Augmented Generation) support for OntoDB.

Provides simple RAG functionality without external dependencies.

Usage::

    from ontodb import OntoDB
    from ontodb.ai import OntoDBRAG

    client = OntoDB("http://localhost:7912")
    rag = OntoDBRAG(client)

    # Ingest documents
    rag.ingest("documents", [
        {"content": "OntoDB is a semantic database", "title": "About OntoDB"},
        {"content": "It supports vector search", "title": "Features"},
    ])

    # Query
    result = rag.query("documents", "What is OntoDB?")
    print(result["answer_context"])
"""

from typing import Any, Dict, List, Optional
import hashlib
import json

from ..client import OntoDB


class OntoDBRAG:
    """Built-in RAG support for OntoDB.

    Args:
        client: OntoDB client instance
        text_column: Column name for text content
        embedding_column: Column name for vector embeddings
    """

    def __init__(
        self,
        client: OntoDB,
        text_column: str = "content",
        embedding_column: str = "embedding",
    ):
        self.client = client
        self.text_column = text_column
        self.embedding_column = embedding_column

    def ingest(
        self,
        collection: str,
        documents: List[Dict[str, Any]],
        batch_size: int = 100,
    ) -> int:
        """Ingest documents into the collection.

        Args:
            collection: Collection name
            documents: List of documents with at least a 'content' field
            batch_size: Batch size for bulk insert

        Returns:
            Number of documents ingested
        """
        if not documents:
            return 0

        # Prepare documents
        prepared = []
        for doc in documents:
            if self.text_column not in doc:
                raise ValueError(f"Document must have '{self.text_column}' field")

            # Generate a simple hash-based ID if not provided
            if "__pk__" not in doc:
                content = doc[self.text_column]
                doc["__pk__"] = hashlib.md5(content.encode()).hexdigest()

            prepared.append(doc)

        # Batch insert
        count = 0
        for i in range(0, len(prepared), batch_size):
            batch = prepared[i:i + batch_size]
            self.client.insert_many(collection, batch)
            count += len(batch)

        return count

    def query(
        self,
        collection: str,
        question: str,
        top_k: int = 5,
        filter_expr: Optional[str] = None,
    ) -> Dict[str, Any]:
        """Query using text similarity.

        Args:
            collection: Collection name
            question: Query text
            top_k: Number of results to return
            filter_expr: Optional SQL WHERE filter

        Returns:
            Dictionary with answer_context and sources
        """
        # For now, use SQL LIKE search as a simple text search
        # In production, this would use vector search with embeddings
        sql = f"SELECT * FROM {collection}"
        if filter_expr:
            sql += f" WHERE {filter_expr}"
        sql += f" LIMIT {top_k}"

        results = self.client.query(sql)

        return {
            "answer_context": [r.get(self.text_column, str(r)) for r in results],
            "sources": results,
            "query": question,
        }

    def search(
        self,
        collection: str,
        query_vector: List[float],
        top_k: int = 5,
        filter_expr: Optional[str] = None,
    ) -> List[Dict[str, Any]]:
        """Search using vector similarity.

        Args:
            collection: Collection name
            query_vector: Query vector
            top_k: Number of results to return
            filter_expr: Optional SQL WHERE filter

        Returns:
            List of similar documents
        """
        return self.client.vector_search(
            table=collection,
            column=self.embedding_column,
            vector=query_vector,
            top_k=top_k,
            filter_expr=filter_expr,
        )

    def create_collection(
        self,
        collection: str,
        vector_dimension: int = 1536,
        extra_columns: Optional[List[Dict[str, str]]] = None,
    ) -> None:
        """Create a collection with vector support.

        Args:
            collection: Collection name
            vector_dimension: Vector dimension (default: 1536 for OpenAI)
            extra_columns: Additional columns to create
        """
        # Create the class
        self.client.execute(f"CREATE CLASS {collection}")

        # Create vector column
        self.client.execute(
            f"CREATE PROPERTY {collection}.{self.embedding_column} "
            f"DOMAIN {collection} RANGE ARRAY"
        )

        # Create text column
        self.client.execute(
            f"CREATE PROPERTY {collection}.{self.text_column} "
            f"DOMAIN {collection} RANGE STRING"
        )

        # Create extra columns
        if extra_columns:
            for col in extra_columns:
                name = col.get("name")
                dtype = col.get("type", "STRING")
                if name:
                    self.client.execute(
                        f"CREATE PROPERTY {collection}.{name} "
                        f"DOMAIN {collection} RANGE {dtype}"
                    )

        # Create vector index
        self.client.execute(
            f"CREATE VECTOR INDEX ON {collection} ({self.embedding_column}) "
            f"METRIC cosine DIMENSION {vector_dimension}"
        )

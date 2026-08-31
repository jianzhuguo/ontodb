"""
LangChain Vector Store adapter for OntoDB.

Provides OntoDBVectorStore class that integrates with LangChain's vector store interface.

Usage::

    from langchain.embeddings import OpenAIEmbeddings
    from ontodb import OntoDB
    from ontodb.ai.langchain import OntoDBVectorStore

    # Initialize
    client = OntoDB("http://localhost:7912")
    embeddings = OpenAIEmbeddings()
    
    # Create vector store
    store = OntoDBVectorStore(
        client=client,
        collection="documents",
        embedding=embeddings,
        text_column="content",
        embedding_column="embedding",
    )

    # Add documents
    store.add_texts(
        ["Hello world", "OntoDB is great"],
        metadatas=[{"source": "test"}, {"source": "docs"}]
    )

    # Search
    results = store.similarity_search("database", k=5)
"""

from typing import Any, Dict, List, Optional, Tuple
from langchain_core.documents import Document
from langchain_core.embeddings import Embeddings
from langchain_core.vectorstores import VectorStore

from ...client import OntoDB


class OntoDBVectorStore(VectorStore):
    """LangChain vector store backed by OntoDB.

    This class implements LangChain's VectorStore interface,
    allowing OntoDB to be used as a vector store in LangChain pipelines.

    Args:
        client: OntoDB client instance
        collection: Collection/table name
        embedding: Embedding model instance
        text_column: Column name for text content
        embedding_column: Column name for vector embeddings
        metadata_columns: Additional columns to include in metadata
    """

    def __init__(
        self,
        client: OntoDB,
        collection: str,
        embedding: Embeddings,
        text_column: str = "content",
        embedding_column: str = "embedding",
        metadata_columns: Optional[List[str]] = None,
    ):
        self.client = client
        self.collection = collection
        self.embedding = embedding
        self.text_column = text_column
        self.embedding_column = embedding_column
        self.metadata_columns = metadata_columns or []

    def add_texts(
        self,
        texts: Iterable[str],
        metadatas: Optional[List[dict]] = None,
        **kwargs: Any,
    ) -> List[str]:
        """Embed texts and store in OntoDB.

        Args:
            texts: Texts to embed and store
            metadatas: Optional metadata for each text
            **kwargs: Additional arguments

        Returns:
            List of document IDs
        """
        texts = list(texts)
        if not texts:
            return []

        # Generate embeddings
        embeddings = self.embedding.embed_documents(texts)

        # Prepare documents
        ids = []
        for i, (text, embedding) in enumerate(zip(texts, embeddings)):
            doc = {
                self.text_column: text,
                self.embedding_column: embedding,
            }
            if metadatas and i < len(metadatas):
                doc.update(metadatas[i])

            # Insert into OntoDB
            result = self.client.insert(self.collection, doc)
            ids.append(result.get("__pk__", str(i)))

        return ids

    def similarity_search(
        self,
        query: str,
        k: int = 4,
        filter_expr: Optional[str] = None,
        **kwargs: Any,
    ) -> List[Document]:
        """Search for similar documents.

        Args:
            query: Query text
            k: Number of results to return
            filter_expr: Optional SQL WHERE filter
            **kwargs: Additional arguments

        Returns:
            List of similar documents
        """
        results = self.similarity_search_with_score(query, k=k, filter_expr=filter_expr, **kwargs)
        return [doc for doc, _ in results]

    def similarity_search_with_score(
        self,
        query: str,
        k: int = 4,
        filter_expr: Optional[str] = None,
        **kwargs: Any,
    ) -> List[Tuple[Document, float]]:
        """Search for similar documents with scores.

        Args:
            query: Query text
            k: Number of results to return
            filter_expr: Optional SQL WHERE filter
            **kwargs: Additional arguments

        Returns:
            List of (document, score) tuples
        """
        # Generate query embedding
        query_embedding = self.embedding.embed_query(query)

        # Search in OntoDB
        results = self.client.vector_search(
            table=self.collection,
            column=self.embedding_column,
            vector=query_embedding,
            top_k=k,
            filter_expr=filter_expr,
        )

        # Convert to LangChain documents
        documents = []
        for row in results:
            # Extract text content
            text = row.get(self.text_column, "")

            # Extract metadata
            metadata = {}
            for col in self.metadata_columns:
                if col in row:
                    metadata[col] = row[col]

            # Add internal metadata
            metadata["__pk__"] = row.get("__pk__", "")
            metadata["__class__"] = row.get("__class__", "")

            # Get similarity score
            score = row.get("_distance", 0.0)

            doc = Document(page_content=text, metadata=metadata)
            documents.append((doc, score))

        return documents

    def delete(self, ids: List[str], **kwargs: Any) -> Optional[bool]:
        """Delete documents by IDs.

        Args:
            ids: Document IDs to delete
            **kwargs: Additional arguments

        Returns:
            True if successful
        """
        for doc_id in ids:
            self.client.delete(self.collection, doc_id)
        return True

    @classmethod
    def from_texts(
        cls,
        texts: List[str],
        embedding: Embeddings,
        metadatas: Optional[List[dict]] = None,
        **kwargs: Any,
    ) -> "OntoDBVectorStore":
        """Create a vector store from texts.

        Args:
            texts: Texts to store
            embedding: Embedding model
            metadatas: Optional metadata
            **kwargs: Additional arguments (including client, collection)

        Returns:
            OntoDBVectorStore instance
        """
        client = kwargs.get("client")
        collection = kwargs.get("collection", "documents")
        
        if not client:
            raise ValueError("client parameter is required")

        store = cls(client=client, collection=collection, embedding=embedding)
        store.add_texts(texts, metadatas=metadatas)
        return store

    @classmethod
    def from_documents(
        cls,
        documents: List[Document],
        embedding: Embeddings,
        **kwargs: Any,
    ) -> "OntoDBVectorStore":
        """Create a vector store from documents.

        Args:
            documents: Documents to store
            embedding: Embedding model
            **kwargs: Additional arguments

        Returns:
            OntoDBVectorStore instance
        """
        texts = [doc.page_content for doc in documents]
        metadatas = [doc.metadata for doc in documents]
        return cls.from_texts(texts, embedding, metadatas=metadatas, **kwargs)

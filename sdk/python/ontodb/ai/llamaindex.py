"""
LlamaIndex adapter for OntoDB.

Provides reader and vector store classes that integrate with LlamaIndex.

Usage::

    from llama_index.core import VectorStoreIndex, StorageContext
    from ontodb import OntoDB
    from ontodb.ai.llamaindex import OntoDBVectorStore, OntoDBReader

    client = OntoDB("http://localhost:7912")

    # As a Reader
    reader = OntoDBReader(client, collection="documents")
    documents = reader.load_data()

    # As a Vector Store
    store = OntoDBVectorStore(client=client, collection="documents")
    storage_context = StorageContext.from_defaults(vector_store=store)
    index = VectorStoreIndex.from_documents(documents, storage_context=storage_context)
"""

from typing import Any, Dict, List, Optional, Sequence
from llama_index.core.base.base_query_engine import BaseQueryEngine
from llama_index.core.base.embeddings.base import BaseEmbedding
from llama_index.core.readers.base import BaseReader
from llama_index.core.schema import BaseNode, Document, TextNode, NodeWithScore
from llama_index.core.vector_stores.types import (
    BasePydanticVectorStore,
    VectorStore,
    VectorStoreQuery,
    VectorStoreQueryResult,
)
from llama_index.core.vector_stores.utils import (
    metadata_dict_to_node,
    node_to_metadata_dict,
)

from ..client import OntoDB


class OntoDBReader(BaseReader):
    """LlamaIndex reader for OntoDB.

    Reads documents from an OntoDB collection.

    Args:
        client: OntoDB client instance
        collection: Collection/table name
        text_column: Column name for text content
        metadata_columns: Additional columns to include in metadata
    """

    def __init__(
        self,
        client: OntoDB,
        collection: str,
        text_column: str = "content",
        metadata_columns: Optional[List[str]] = None,
    ):
        self.client = client
        self.collection = collection
        self.text_column = text_column
        self.metadata_columns = metadata_columns or []

    def load_data(
        self,
        query: Optional[str] = None,
        limit: Optional[int] = None,
        **kwargs,
    ) -> List[Document]:
        """Load documents from OntoDB.

        Args:
            query: Optional SQL query to filter documents
            limit: Maximum number of documents to return
            **kwargs: Additional arguments

        Returns:
            List of LlamaIndex Document objects
        """
        if query:
            sql = query
        else:
            sql = f"SELECT * FROM {self.collection}"
            if limit:
                sql += f" LIMIT {limit}"

        results = self.client.query(sql)

        documents = []
        for row in results:
            # Extract text content
            text = row.get(self.text_column, "")
            if not text:
                continue

            # Extract metadata
            metadata = {}
            for col in self.metadata_columns:
                if col in row:
                    metadata[col] = row[col]

            # Add internal metadata
            metadata["__pk__"] = row.get("__pk__", "")
            metadata["__class__"] = row.get("__class__", "")
            metadata["collection"] = self.collection

            doc = Document(
                text=text,
                metadata=metadata,
                id_=row.get("__pk__"),
            )
            documents.append(doc)

        return documents


class OntoDBVectorStore(BasePydanticVectorStore):
    """LlamaIndex vector store backed by OntoDB.

    This class implements LlamaIndex's BasePydanticVectorStore interface,
    allowing OntoDB to be used as a vector store in LlamaIndex pipelines.

    Args:
        client: OntoDB client instance
        collection: Collection/table name
        text_column: Column name for text content
        embedding_column: Column name for vector embeddings
        metadata_columns: Additional columns to include in metadata
    """

    stores_text: bool = True
    flat_metadata: bool = True

    client: Any = None
    collection: str = "documents"
    text_column: str = "content"
    embedding_column: str = "embedding"
    metadata_columns: List[str] = []

    class Config:
        arbitrary_types_allowed = True

    def __init__(
        self,
        client: OntoDB,
        collection: str = "documents",
        text_column: str = "content",
        embedding_column: str = "embedding",
        metadata_columns: Optional[List[str]] = None,
        **kwargs,
    ):
        super().__init__(
            client=client,
            collection=collection,
            text_column=text_column,
            embedding_column=embedding_column,
            metadata_columns=metadata_columns or [],
            **kwargs,
        )

    @property
    def client(self) -> OntoDB:
        return self._client

    @client.setter
    def client(self, value: OntoDB):
        self._client = value

    def add(
        self,
        nodes: List[BaseNode],
        **add_kwargs,
    ) -> List[str]:
        """Add nodes to the vector store.

        Args:
            nodes: List of nodes to add
            **add_kwargs: Additional arguments

        Returns:
            List of node IDs
        """
        ids = []
        for node in nodes:
            # Extract text and embedding
            text = node.get_content()
            embedding = node.embedding

            # Build document
            doc = {
                self.text_column: text,
            }
            if embedding is not None:
                doc[self.embedding_column] = embedding

            # Add metadata
            metadata = node_to_metadata_dict(node, remove_text=True, flat_metadata=self.flat_metadata)
            doc.update(metadata)

            # Insert into OntoDB
            result = self._client.insert(self.collection, doc)
            ids.append(result.get("__pk__", node.node_id))

        return ids

    def delete(
        self,
        ref_doc_id: str,
        **delete_kwargs,
    ) -> None:
        """Delete a node by ID.

        Args:
            ref_doc_id: Document ID to delete
            **delete_kwargs: Additional arguments
        """
        self._client.delete(self.collection, ref_doc_id)

    def query(
        self,
        query: VectorStoreQuery,
        **kwargs,
    ) -> VectorStoreQueryResult:
        """Query the vector store.

        Args:
            query: Vector store query
            **kwargs: Additional arguments

        Returns:
            VectorStoreQueryResult with nodes and similarities
        """
        if query.query_embedding is None:
            raise ValueError("Query embedding is required")

        # Search in OntoDB
        results = self._client.vector_search(
            table=self.collection,
            column=self.embedding_column,
            vector=query.query_embedding,
            top_k=query.similarity_top_k,
            filter_expr=query.doc_ids[0] if query.doc_ids else None,
        )

        # Convert to LlamaIndex nodes
        nodes = []
        similarities = []
        for row in results:
            # Extract text
            text = row.get(self.text_column, "")

            # Extract metadata
            metadata = {}
            for col in self.metadata_columns:
                if col in row:
                    metadata[col] = row[col]
            metadata["__pk__"] = row.get("__pk__", "")
            metadata["__class__"] = row.get("__class__", "")

            # Create node
            node = TextNode(
                text=text,
                metadata=metadata,
                id_=row.get("__pk__", ""),
            )

            # Get similarity score
            score = row.get("_distance", 0.0)

            nodes.append(node)
            similarities.append(score)

        return VectorStoreQueryResult(
            nodes=nodes,
            similarities=similarities,
        )

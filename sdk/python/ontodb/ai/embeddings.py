"""
Embedding utilities for OntoDB AI integrations.

Provides helper classes for generating embeddings.

Usage::

    from ontodb.ai.embeddings import OntoDBEmbeddings

    # Use default embedding (hash-based, for testing)
    embeddings = OntoDBEmbeddings()
    vector = embeddings.embed_query("Hello world")

    # Use custom embedding model
    from ontodb.ai.embeddings import OpenAIEmbeddings
    embeddings = OpenAIEmbeddings(api_key="your-key")
    vector = embeddings.embed_query("Hello world")
"""

from typing import List, Optional
import hashlib
import struct


class OntoDBEmbeddings:
    """Default embedding model for OntoDB.

    This is a simple hash-based embedding for testing and development.
    For production, use a proper embedding model like OpenAI, Cohere, etc.

    Args:
        dimension: Output vector dimension (default: 128)
    """

    def __init__(self, dimension: int = 128):
        self.dimension = dimension

    def embed_documents(self, texts: List[str]) -> List[List[float]]:
        """Embed multiple documents.

        Args:
            texts: List of texts to embed

        Returns:
            List of embedding vectors
        """
        return [self.embed_query(text) for text in texts]

    def embed_query(self, text: str) -> List[float]:
        """Embed a single query text.

        Args:
            text: Text to embed

        Returns:
            Embedding vector
        """
        # Simple hash-based embedding for testing
        # In production, use a proper embedding model
        hash_bytes = hashlib.sha256(text.encode()).digest()

        # Convert to float vector
        vector = []
        for i in range(0, len(hash_bytes), 4):
            if len(vector) >= self.dimension:
                break
            chunk = hash_bytes[i:i + 4]
            if len(chunk) == 4:
                # Convert 4 bytes to float between -1 and 1
                val = struct.unpack('f', chunk)[0]
                # Normalize to [-1, 1] range
                val = max(-1.0, min(1.0, val / 1e30))
                vector.append(val)

        # Pad or truncate to desired dimension
        while len(vector) < self.dimension:
            vector.append(0.0)

        return vector[:self.dimension]


class OpenAIEmbeddings:
    """OpenAI embedding model wrapper.

    Args:
        api_key: OpenAI API key
        model: Model name (default: "text-embedding-ada-002")
    """

    def __init__(self, api_key: str, model: str = "text-embedding-ada-002"):
        self.api_key = api_key
        self.model = model
        self._client = None

    def _get_client(self):
        """Lazy initialize OpenAI client."""
        if self._client is None:
            try:
                import openai
                self._client = openai.OpenAI(api_key=self.api_key)
            except ImportError:
                raise ImportError(
                    "openai package is required for OpenAI embeddings. "
                    "Install it with: pip install openai"
                )
        return self._client

    def embed_documents(self, texts: List[str]) -> List[List[float]]:
        """Embed multiple documents using OpenAI.

        Args:
            texts: List of texts to embed

        Returns:
            List of embedding vectors
        """
        client = self._get_client()
        response = client.embeddings.create(
            model=self.model,
            input=texts,
        )
        return [item.embedding for item in response.data]

    def embed_query(self, text: str) -> List[float]:
        """Embed a single query text using OpenAI.

        Args:
            text: Text to embed

        Returns:
            Embedding vector
        """
        client = self._get_client()
        response = client.embeddings.create(
            model=self.model,
            input=[text],
        )
        return response.data[0].embedding


class CohereEmbeddings:
    """Cohere embedding model wrapper.

    Args:
        api_key: Cohere API key
        model: Model name (default: "embed-english-v2.0")
    """

    def __init__(self, api_key: str, model: str = "embed-english-v2.0"):
        self.api_key = api_key
        self.model = model
        self._client = None

    def _get_client(self):
        """Lazy initialize Cohere client."""
        if self._client is None:
            try:
                import cohere
                self._client = cohere.Client(self.api_key)
            except ImportError:
                raise ImportError(
                    "cohere package is required for Cohere embeddings. "
                    "Install it with: pip install cohere"
                )
        return self._client

    def embed_documents(self, texts: List[str]) -> List[List[float]]:
        """Embed multiple documents using Cohere.

        Args:
            texts: List of texts to embed

        Returns:
            List of embedding vectors
        """
        client = self._get_client()
        response = client.embed(texts=texts, model=self.model)
        return response.embeddings

    def embed_query(self, text: str) -> List[float]:
        """Embed a single query text using Cohere.

        Args:
            text: Text to embed

        Returns:
            Embedding vector
        """
        client = self._get_client()
        response = client.embed(texts=[text], model=self.model)
        return response.embeddings[0]

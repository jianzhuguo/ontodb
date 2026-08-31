"""
OntoDB AI Integrations
======================

AI framework adapters for OntoDB.

Supported frameworks:
- LangChain (vector store, retriever, memory)
- LlamaIndex (reader, vector store)
- Built-in RAG support

Usage::

    from ontodb.ai import OntoDBVectorStore, OntoDBRAG

    # LangChain integration
    from ontodb.ai.langchain import OntoDBVectorStore
    
    # LlamaIndex integration
    from ontodb.ai.llamaindex import OntoDBReader, OntoDBVectorStore
    
    # Built-in RAG
    rag = OntoDBRAG(client)
    rag.ingest("documents", docs)
    result = rag.query("documents", "What is OntoDB?")
"""

from .rag import OntoDBRAG
from .embeddings import OntoDBEmbeddings

__all__ = [
    "OntoDBRAG",
    "OntoDBEmbeddings",
]

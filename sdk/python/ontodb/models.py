"""OntoDB SDK data models."""

from dataclasses import dataclass, field
from typing import Any, Dict, List, Optional


@dataclass
class QueryResult:
    """SQL query result."""
    data: List[Dict[str, Any]] = field(default_factory=list)
    rows_affected: int = 0
    elapsed_ms: float = 0.0

    def __iter__(self):
        return iter(self.data)

    def __len__(self):
        return len(self.data)

    def __getitem__(self, index):
        return self.data[index]


@dataclass
class VectorSearchResult:
    """Vector search result item."""
    id: str
    score: float
    data: Dict[str, Any] = field(default_factory=dict)


@dataclass
class MultiVectorSearchResult:
    """Multi-vector search result item."""
    doc_key: str
    combined_score: float
    per_column_scores: Dict[str, float] = field(default_factory=dict)


@dataclass
class ClusterInfo:
    """K-Means cluster information."""
    id: int
    centroid: List[float] = field(default_factory=list)
    member_count: int = 0


@dataclass
class ClusteringResult:
    """K-Means clustering result."""
    clusters: List[ClusterInfo] = field(default_factory=list)
    assignments: List[int] = field(default_factory=list)
    iterations: int = 0
    converged: bool = False


@dataclass
class GraphVertex:
    """Graph vertex."""
    id: str
    label: str
    properties: Dict[str, Any] = field(default_factory=dict)


@dataclass
class GraphEdge:
    """Graph edge."""
    id: str
    from_id: str
    to_id: str
    label: str
    properties: Dict[str, Any] = field(default_factory=dict)


@dataclass
class GraphResult:
    """Graph traversal result."""
    vertices: List[GraphVertex] = field(default_factory=list)
    edges: List[GraphEdge] = field(default_factory=list)
    paths: List[List[str]] = field(default_factory=list)


@dataclass
class ColumnInfo:
    """Column information."""
    name: str
    data_type: str
    nullable: bool = True
    default: Optional[str] = None


@dataclass
class TableInfo:
    """Table information."""
    name: str
    columns: List[ColumnInfo] = field(default_factory=list)
    row_count: int = 0


@dataclass
class SchemaInfo:
    """Database schema information."""
    tables: List[TableInfo] = field(default_factory=list)
    version: str = ""

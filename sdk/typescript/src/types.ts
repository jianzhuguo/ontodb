/** OntoDB SDK Type Definitions */

/** Standard API response wrapper */
export interface ApiResponse<T = unknown> {
  success: boolean;
  data?: T;
  error?: string;
  elapsed_ms?: number;
}

/** Query request */
export interface QueryRequest {
  query: string;
  pretty?: boolean;
}

/** SPARQL query request */
export interface SparqlRequest {
  query: string;
}

/** Vector search request */
export interface VectorSearchRequest {
  class: string;
  column: string;
  query_vector: number[];
  top_k: number;
  filter?: string;
}

/** Hybrid SQL + vector search request */
export interface HybridQueryRequest {
  sql_filter: string;
  vector_column: string;
  query_vector: number[];
  top_k: number;
  class?: string;
}

/** Graph vertex */
export interface Vertex {
  id: string;
  labels?: string[];
  properties?: Record<string, unknown>;
}

/** Graph edge */
export interface Edge {
  id: string;
  from: string;
  to: string;
  label?: string;
  properties?: Record<string, unknown>;
}

/** Graph traversal request */
export interface TraverseRequest {
  start: string;
  direction?: 'out' | 'in' | 'both';
  max_depth?: number;
  edge_label?: string;
  algorithm?: 'bfs' | 'dfs';
}

/** Shortest path request */
export interface ShortestPathRequest {
  from: string;
  to: string;
  max_depth?: number;
}

/** Shortest path result */
export interface ShortestPathResult {
  from: string;
  to: string;
  found: boolean;
  length?: number;
  path?: {
    vertex_ids: string[];
    edge_ids: string[];
  };
  message?: string;
}

/** Traversal result */
export interface TraversalResult {
  start: string;
  direction: string;
  max_depth: number;
  algorithm: string;
  visited_count: number;
  vertices: Vertex[];
}

/** Health check response */
export interface HealthResponse {
  status: 'ok' | 'degraded';
  version: string;
  engine: string;
  uptime_seconds: number;
  checks: Record<string, unknown>;
}

/** Backup request */
export interface BackupRequest {
  path: string;
}

/** Incremental backup request */
export interface IncrementalBackupRequest {
  path: string;
  since: string;
}

/** Backup result */
export interface BackupResult {
  message: string;
  path: string;
  files: number;
  total_bytes: number;
  timestamp: string;
  backup_type?: string;
}

/** Client configuration */
export interface OntoDBClientOptions {
  /** Server base URL (default: http://localhost:7912) */
  baseUrl?: string;
  /** API key for authentication */
  apiKey?: string;
  /** Request timeout in milliseconds (default: 30000) */
  timeout?: number;
}

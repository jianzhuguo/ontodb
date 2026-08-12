/** OntoDB client options */
export interface OntoDBOptions {
  /** API key for authentication */
  apiKey?: string;
  /** Request timeout in milliseconds (default: 30000) */
  timeout?: number;
  /** Maximum retries on failure (default: 3) */
  maxRetries?: number;
  /** Custom headers to include in every request */
  headers?: Record<string, string>;
}

/** Query result */
export type QueryResult<T = Record<string, unknown>> = T[];

/** Vector search options */
export interface VectorSearchOptions {
  /** Number of results to return (default: 10) */
  topK?: number;
  /** SQL WHERE filter */
  filter?: string;
  /** Request timeout in ms */
  timeout?: number;
}

/** Graph traversal options */
export interface GraphTraverseOptions {
  /** Traversal direction */
  direction?: 'in' | 'out' | 'both';
  /** Filter by edge label */
  edgeLabel?: string;
  /** Maximum depth (default: 3) */
  depth?: number;
  /** Algorithm: 'bfs' or 'dfs' */
  algorithm?: 'bfs' | 'dfs';
  /** Request timeout in ms */
  timeout?: number;
}

/** Health status response */
export interface HealthStatus {
  status: string;
  version?: string;
  uptime?: number;
}

/** Metrics response */
export interface MetricsInfo {
  queries_total?: number;
  writes_total?: number;
  storage_bytes?: number;
  connections_active?: number;
}

/** Schema info */
export interface SchemaInfo {
  tables?: TableInfo[];
}

/** Table info */
export interface TableInfo {
  name: string;
  columns?: ColumnInfo[];
}

/** Column info */
export interface ColumnInfo {
  name: string;
  type: string;
  nullable?: boolean;
}

/** API response wrapper */
export interface ApiResponse<T = unknown> {
  success?: boolean;
  data?: T;
  error?: string;
  rows_affected?: number;
  elapsed_ms?: number;
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

/** Detailed health response */
export interface HealthResponse {
  status: 'ok' | 'degraded';
  version: string;
  engine: string;
  uptime_seconds: number;
  checks: Record<string, unknown>;
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

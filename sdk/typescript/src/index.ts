/**
 * OntoDB TypeScript/JavaScript SDK
 *
 * Official client library for the OntoDB HTTP API.
 * Supports SQL queries, SPARQL, vector search, and property graph operations.
 *
 * @example
 * ```ts
 * import { OntoDBClient } from 'ontodb';
 *
 * const db = new OntoDBClient({ baseUrl: 'http://localhost:7912' });
 *
 * // SQL query
 * const result = await db.query('SELECT * FROM Product WHERE price > 100');
 *
 * // Vector search
 * const similar = await db.vectorSearch({
 *   class: 'Product',
 *   column: 'embedding',
 *   query_vector: [0.1, 0.2, 0.3],
 *   top_k: 10,
 * });
 *
 * // Graph operations
 * await db.addVertex({ id: 'v1', labels: ['Person'], properties: { name: 'Alice' } });
 * const neighbors = await db.getNeighbors('v1');
 * ```
 */

import type {
  ApiResponse,
  QueryRequest,
  SparqlRequest,
  VectorSearchRequest,
  HybridQueryRequest,
  Vertex,
  Edge,
  TraverseRequest,
  ShortestPathResult,
  TraversalResult,
  HealthResponse,
  BackupRequest,
  IncrementalBackupRequest,
  BackupResult,
  OntoDBClientOptions,
} from './types.js';

export type {
  ApiResponse,
  QueryRequest,
  SparqlRequest,
  VectorSearchRequest,
  HybridQueryRequest,
  Vertex,
  Edge,
  TraverseRequest,
  ShortestPathResult,
  TraversalResult,
  HealthResponse,
  BackupRequest,
  IncrementalBackupRequest,
  BackupResult,
  OntoDBClientOptions,
};

export class OntoDBClient {
  private baseUrl: string;
  private apiKey?: string;
  private timeout: number;

  constructor(options: OntoDBClientOptions = {}) {
    this.baseUrl = (options.baseUrl || 'http://localhost:7912').replace(/\/$/, '');
    this.apiKey = options.apiKey;
    this.timeout = options.timeout || 30000;
  }

  /** Internal fetch wrapper with auth and timeout */
  private async fetch<T>(
    path: string,
    options: RequestInit = {},
  ): Promise<ApiResponse<T>> {
    const url = `${this.baseUrl}${path}`;
    const headers: Record<string, string> = {
      'Content-Type': 'application/json',
      ...((options.headers as Record<string, string>) || {}),
    };

    if (this.apiKey) {
      headers['X-API-Key'] = this.apiKey;
    }

    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), this.timeout);

    try {
      const response = await fetch(url, {
        ...options,
        headers,
        signal: controller.signal,
      });

      const body = await response.json() as ApiResponse<T>;

      if (!response.ok || !body.success) {
        throw new OntoDBError(
          body.error || `HTTP ${response.status}`,
          response.status,
        );
      }

      return body;
    } finally {
      clearTimeout(timer);
    }
  }

  // ── Health ─────────────────────────────────────────────────────

  /** Comprehensive health check (probes storage + query engine) */
  async health(): Promise<HealthResponse> {
    const res = await this.fetch<HealthResponse>('/api/health');
    return res.data!;
  }

  /** Kubernetes readiness probe */
  async ready(): Promise<boolean> {
    try {
      await this.fetch('/api/health/ready');
      return true;
    } catch {
      return false;
    }
  }

  /** Kubernetes liveness probe */
  async alive(): Promise<boolean> {
    try {
      await this.fetch('/api/health/live');
      return true;
    } catch {
      return false;
    }
  }

  // ── SQL Query ──────────────────────────────────────────────────

  /** Execute a SQL query */
  async query<T = Record<string, unknown>[]>(
    sql: string,
    options: { pretty?: boolean } = {},
  ): Promise<ApiResponse<T>> {
    return this.fetch<T>('/api/query', {
      method: 'POST',
      body: JSON.stringify({ query: sql, pretty: options.pretty }),
    });
  }

  // ── SPARQL ─────────────────────────────────────────────────────

  /** Execute a SPARQL query */
  async sparql<T = unknown>(query: string): Promise<ApiResponse<T>> {
    return this.fetch<T>('/sparql', {
      method: 'POST',
      body: JSON.stringify({ query }),
    });
  }

  // ── Vector Search ──────────────────────────────────────────────

  /** Perform vector similarity search */
  async vectorSearch<T = Record<string, unknown>[]>(
    request: VectorSearchRequest,
  ): Promise<ApiResponse<T>> {
    return this.fetch<T>('/api/vector/search', {
      method: 'POST',
      body: JSON.stringify(request),
    });
  }

  /** Perform hybrid SQL + vector search */
  async hybridSearch<T = Record<string, unknown>[]>(
    request: HybridQueryRequest,
  ): Promise<ApiResponse<T>> {
    return this.fetch<T>('/api/hybrid/query', {
      method: 'POST',
      body: JSON.stringify(request),
    });
  }

  // ── Graph Operations ──────────────────────────────────────────

  /** Add a vertex to the graph */
  async addVertex(vertex: Vertex): Promise<ApiResponse> {
    return this.fetch('/api/graph/vertex', {
      method: 'POST',
      body: JSON.stringify(vertex),
    });
  }

  /** Get a vertex by ID */
  async getVertex(id: string): Promise<Vertex> {
    const res = await this.fetch<Vertex>(`/api/graph/vertex/${encodeURIComponent(id)}`);
    return res.data!;
  }

  /** Delete a vertex and its connected edges */
  async deleteVertex(id: string): Promise<void> {
    await this.fetch(`/api/graph/vertex/${encodeURIComponent(id)}`, {
      method: 'DELETE',
    });
  }

  /** Add an edge to the graph */
  async addEdge(edge: Edge): Promise<ApiResponse> {
    return this.fetch('/api/graph/edge', {
      method: 'POST',
      body: JSON.stringify(edge),
    });
  }

  /** Get neighbors of a vertex */
  async getNeighbors(id: string): Promise<{ vertex_id: string; count: number; neighbors: Vertex[] }> {
    const res = await this.fetch<{ vertex_id: string; count: number; neighbors: Vertex[] }>(
      `/api/graph/neighbors/${encodeURIComponent(id)}`,
    );
    return res.data!;
  }

  /** Traverse the graph (BFS or DFS) */
  async traverse(request: TraverseRequest): Promise<TraversalResult> {
    const res = await this.fetch<TraversalResult>('/api/graph/traverse', {
      method: 'POST',
      body: JSON.stringify(request),
    });
    return res.data!;
  }

  /** Find shortest path between two vertices */
  async shortestPath(from: string, to: string, maxDepth = 10): Promise<ShortestPathResult> {
    const res = await this.fetch<ShortestPathResult>('/api/graph/shortest-path', {
      method: 'POST',
      body: JSON.stringify({ from, to, max_depth: maxDepth }),
    });
    return res.data!;
  }

  // ── Schema ─────────────────────────────────────────────────────

  /** Get database schema information */
  async schema<T = unknown>(): Promise<T> {
    const res = await this.fetch<T>('/api/schema');
    return res.data!;
  }

  // ── Admin ──────────────────────────────────────────────────────

  /** Create a full backup */
  async backup(path: string): Promise<BackupResult> {
    const res = await this.fetch<BackupResult>('/api/backup', {
      method: 'POST',
      body: JSON.stringify({ path }),
    });
    return res.data!;
  }

  /** Create an incremental backup (only files changed since the given time) */
  async backupIncremental(path: string, since: string): Promise<BackupResult> {
    const res = await this.fetch<BackupResult>('/api/backup/incremental', {
      method: 'POST',
      body: JSON.stringify({ path, since }),
    });
    return res.data!;
  }

  /** Verify a backup's integrity */
  async verifyBackup(path: string): Promise<{ message: string }> {
    const res = await this.fetch<{ message: string }>('/api/backup/verify', {
      method: 'POST',
      body: JSON.stringify({ path }),
    });
    return res.data!;
  }

  /** Flush MemTable to SSTable */
  async flush(): Promise<void> {
    await this.fetch('/api/flush', { method: 'POST' });
  }
}

/** Custom error class for OntoDB API errors */
export class OntoDBError extends Error {
  constructor(
    message: string,
    public statusCode?: number,
  ) {
    super(message);
    this.name = 'OntoDBError';
  }
}

import type {
  OntoDBOptions,
  QueryResult,
  VectorSearchOptions,
  GraphTraverseOptions,
  HealthStatus,
  SchemaInfo,
  ApiResponse,
} from './types';
import {
  OntoDBError,
  ConnectionError,
  QueryError,
  AuthenticationError,
  TimeoutError,
} from './errors';

/**
 * OntoDB client for JavaScript/TypeScript.
 *
 * Works in Node.js (16+) and modern browsers.
 *
 * @example
 * ```ts
 * const db = new OntoDB('http://localhost:7912', { apiKey: 'your-key' });
 * const rows = await db.query('SELECT * FROM users');
 * ```
 */
export class OntoDB {
  private readonly baseUrl: string;
  private readonly apiKey?: string;
  private readonly timeout: number;
  private readonly maxRetries: number;
  private readonly defaultHeaders: Record<string, string>;

  constructor(baseUrl: string, options: OntoDBOptions = {}) {
    this.baseUrl = baseUrl.replace(/\/+$/, '');
    this.apiKey = options.apiKey;
    this.timeout = options.timeout ?? 30_000;
    this.maxRetries = options.maxRetries ?? 3;
    this.defaultHeaders = {
      'Content-Type': 'application/json',
      ...options.headers,
    };
    if (this.apiKey) {
      this.defaultHeaders['Authorization'] = `Bearer ${this.apiKey}`;
    }
  }

  private async request<T = unknown>(
    method: string,
    path: string,
    body?: unknown,
    timeout?: number,
  ): Promise<ApiResponse<T>> {
    const url = `${this.baseUrl}${path}`;
    let lastError: Error | undefined;

    for (let attempt = 0; attempt <= this.maxRetries; attempt++) {
      try {
        const controller = new AbortController();
        const timer = setTimeout(
          () => controller.abort(),
          timeout ?? this.timeout,
        );

        const response = await fetch(url, {
          method,
          headers: this.defaultHeaders,
          body: body ? JSON.stringify(body) : undefined,
          signal: controller.signal,
        });

        clearTimeout(timer);

        if (response.status === 401) {
          throw new AuthenticationError('Invalid API key');
        }
        if (response.status === 429) {
          throw new OntoDBError('Rate limit exceeded');
        }
        if (response.status >= 400) {
          let msg = `HTTP ${response.status}`;
          try {
            const errBody = await response.json();
            msg = errBody.error || msg;
          } catch {
            msg = (await response.text()) || msg;
          }
          throw new QueryError(msg);
        }

        return (await response.json()) as ApiResponse<T>;
      } catch (err) {
        if (err instanceof AuthenticationError || err instanceof QueryError) {
          throw err;
        }

        if (err instanceof DOMException && err.name === 'AbortError') {
          lastError = new TimeoutError(`Request timed out after ${timeout ?? this.timeout}ms`);
        } else if (err instanceof TypeError && String(err.message).includes('fetch')) {
          lastError = new ConnectionError(`Cannot connect to ${this.baseUrl}: ${err.message}`);
        } else if (err instanceof OntoDBError) {
          throw err;
        } else {
          lastError = new OntoDBError(`Unexpected error: ${err}`);
        }

        if (attempt < this.maxRetries) {
          await new Promise((r) => setTimeout(r, 500 * (attempt + 1)));
          continue;
        }
      }
    }

    throw lastError ?? new OntoDBError('Max retries exceeded');
  }

  // ──────────────────────────────────────────────
  // SQL Queries
  // ──────────────────────────────────────────────

  /**
   * Execute a SQL query and return results.
   *
   * @example
   * ```ts
   * const rows = await db.query('SELECT * FROM users WHERE age > 25');
   * for (const row of rows) {
   *   console.log(row.name, row.age);
   * }
   * ```
   */
  async query<T = Record<string, unknown>>(sql: string, timeout?: number): Promise<QueryResult<T>> {
    const result = await this.request<T[]>('POST', '/api/query', { query: sql }, timeout);
    if (result.error) throw new QueryError(result.error);
    return (result.data ?? []) as QueryResult<T>;
  }

  /**
   * Execute a SQL statement (INSERT/UPDATE/DELETE/DDL).
   *
   * @example
   * ```ts
   * await db.execute("INSERT INTO users (name, age) VALUES ('Alice', 30)");
   * ```
   */
  async execute(sql: string, timeout?: number): Promise<ApiResponse> {
    const result = await this.request('POST', '/api/query', { query: sql }, timeout);
    if (result.error) throw new QueryError(result.error);
    return result;
  }

  /**
   * Execute multiple SQL queries sequentially.
   */
  async queryMany<T = Record<string, unknown>>(sqls: string[], timeout?: number): Promise<QueryResult<T>[]> {
    const results: QueryResult<T>[] = [];
    for (const sql of sqls) {
      results.push(await this.query<T>(sql, timeout));
    }
    return results;
  }

  /**
   * Batch insert multiple rows.
   *
   * @example
   * ```ts
   * await db.insertMany('users', [
   *   { name: 'Alice', age: 30 },
   *   { name: 'Bob', age: 25 },
   * ]);
   * ```
   */
  async insertMany(table: string, rows: Record<string, unknown>[], timeout?: number): Promise<ApiResponse> {
    if (!rows.length) return {};

    const columns = Object.keys(rows[0]);
    const colsStr = columns.join(', ');

    const values = rows.map((row) => {
      const vals = columns.map((col) => {
        const v = row[col];
        if (v === null || v === undefined) return 'NULL';
        if (typeof v === 'string') return `'${v.replace(/'/g, "''")}'`;
        if (typeof v === 'boolean') return v ? 'TRUE' : 'FALSE';
        return String(v);
      });
      return `(${vals.join(', ')})`;
    });

    const sql = `BATCH INSERT INTO ${table} (${colsStr}) VALUES ${values.join(', ')}`;
    return this.execute(sql, timeout);
  }

  // ──────────────────────────────────────────────
  // Vector Search
  // ──────────────────────────────────────────────

  /**
   * Search for similar vectors.
   *
   * @example
   * ```ts
   * const results = await db.vectorSearch('documents', 'embedding', [0.1, 0.2, ...], { topK: 5 });
   * for (const r of results) {
   *   console.log(r.title, r._score);
   * }
   * ```
   */
  async vectorSearch<T = Record<string, unknown>>(
    table: string,
    column: string,
    vector: number[],
    options: VectorSearchOptions = {},
  ): Promise<QueryResult<T & { _score?: number }>> {
    const body: Record<string, unknown> = {
      class: table,
      column,
      query_vector: vector,
      top_k: options.topK ?? 10,
    };
    if (options.filter) body.filter = options.filter;

    const result = await this.request<T[]>('POST', '/api/vector/search', body, options.timeout);
    if (result.error) throw new QueryError(result.error);
    return (result.data ?? []) as QueryResult<T & { _score?: number }>;
  }

  /**
   * Hybrid SQL + vector search.
   */
  async hybridSearch<T = Record<string, unknown>>(
    table: string,
    vectorColumn: string,
    vector: number[],
    filter: string = '',
    topK: number = 10,
    timeout?: number,
  ): Promise<QueryResult<T>> {
    const result = await this.request<T[]>('POST', '/api/hybrid/query', {
      class: table,
      vector_column: vectorColumn,
      query_vector: vector,
      filter,
      top_k: topK,
    }, timeout);
    if (result.error) throw new QueryError(result.error);
    return (result.data ?? []) as QueryResult<T>;
  }

  // ──────────────────────────────────────────────
  // SPARQL
  // ──────────────────────────────────────────────

  /**
   * Execute a SPARQL query.
   *
   * @example
   * ```ts
   * const results = await db.sparql(`
   *   PREFIX ex: <http://example.org/>
   *   SELECT ?name WHERE { ?p ex:name ?name }
   * `);
   * ```
   */
  async sparql<T = Record<string, unknown>>(query: string, timeout?: number): Promise<QueryResult<T>> {
    const result = await this.request<T[]>('POST', '/api/sparql', { query }, timeout);
    if (result.error) throw new QueryError(result.error);
    return (result.data ?? []) as QueryResult<T>;
  }

  // ──────────────────────────────────────────────
  // Graph Operations
  // ──────────────────────────────────────────────

  /**
   * Traverse the graph from a starting vertex.
   *
   * @example
   * ```ts
   * const result = await db.graphTraverse('Person::1', { direction: 'out', depth: 2 });
   * console.log(result.vertices);
   * ```
   */
  async graphTraverse(
    startId: string,
    options: GraphTraverseOptions = {},
  ): Promise<{ vertices?: unknown[]; edges?: unknown[] }> {
    const body: Record<string, unknown> = {
      start: startId,
      direction: options.direction ?? 'out',
      max_depth: options.depth ?? 3,
      algorithm: options.algorithm ?? 'bfs',
    };
    if (options.edgeLabel) body.edge_label = options.edgeLabel;

    const result = await this.request<{ vertices?: unknown[]; edges?: unknown[] }>(
      'POST', '/api/graph/traverse', body, options.timeout,
    );
    if (result.error) throw new QueryError(result.error);
    return result.data ?? {};
  }

  /**
   * Find shortest path between two vertices.
   */
  async graphShortestPath(fromId: string, toId: string, timeout?: number): Promise<string[]> {
    const result = await this.request<{ path?: string[] }>(
      'POST', '/api/graph/shortest-path', { from: fromId, to: toId }, timeout,
    );
    if (result.error) throw new QueryError(result.error);
    return result.data?.path ?? [];
  }

  // ──────────────────────────────────────────────
  // Schema
  // ──────────────────────────────────────────────

  /**
   * Get database schema information.
   */
  async schema(timeout?: number): Promise<SchemaInfo> {
    const result = await this.request<SchemaInfo>('GET', '/api/schema', undefined, timeout);
    return result.data ?? {};
  }

  // ──────────────────────────────────────────────
  // Health & Metrics
  // ──────────────────────────────────────────────

  /**
   * Check server health.
   */
  async health(timeout?: number): Promise<HealthStatus> {
    const result = await this.request<HealthStatus>('GET', '/api/health', undefined, timeout ?? 5000);
    return result as unknown as HealthStatus;
  }

  /**
   * Check if server is ready to accept requests.
   */
  async isReady(): Promise<boolean> {
    try {
      const h = await this.health(2000);
      return h.status === 'ok';
    } catch {
      return false;
    }
  }

  // ──────────────────────────────────────────────
  // Backup
  // ──────────────────────────────────────────────

  /**
   * Create a full backup.
   */
  async backup(path: string, timeout?: number): Promise<ApiResponse> {
    const result = await this.request('POST', '/api/backup', { path }, timeout);
    if (result.error) throw new QueryError(result.error);
    return result;
  }

  /**
   * Restore from backup.
   */
  async restore(path: string, timeout?: number): Promise<ApiResponse> {
    const result = await this.request('POST', '/api/restore', { path }, timeout);
    if (result.error) throw new QueryError(result.error);
    return result;
  }

  toString(): string {
    return `OntoDB('${this.baseUrl}')`;
  }
}

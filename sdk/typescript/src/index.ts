/**
 * OntoDB TypeScript SDK
 *
 * This is a thin wrapper around the canonical `ontodb` JavaScript SDK.
 * It provides the `OntoDBClient` class name for backwards compatibility
 * and re-exports all types and errors.
 *
 * @deprecated Use `ontodb` (the JavaScript SDK) directly for new projects.
 *
 * @example
 * ```ts
 * import { OntoDBClient } from 'ontodb';
 *
 * const db = new OntoDBClient('http://localhost:7912', { apiKey: 'your-key' });
 * const result = await db.query('SELECT * FROM Product WHERE price > 100');
 * ```
 */

import {
  OntoDB,
  OntoDBError,
  ConnectionError,
  QueryError,
  AuthenticationError,
  TimeoutError,
} from '../../javascript/src/index';

import type {
  OntoDBOptions,
  QueryResult,
  VectorSearchOptions,
  GraphTraverseOptions,
  HealthStatus,
  MetricsInfo,
  SchemaInfo,
  ApiResponse,
  Vertex,
  Edge,
  HealthResponse,
  BackupResult,
} from '../../javascript/src/types';

/**
 * OntoDB TypeScript client.
 *
 * Alias for `OntoDB` from the canonical JavaScript SDK.
 * Accepts both constructor styles:
 * - `new OntoDBClient('http://host', { apiKey: 'key' })`
 * - `new OntoDBClient({ baseUrl: 'http://host', apiKey: 'key' })` (deprecated)
 */
export class OntoDBClient extends OntoDB {
  constructor(urlOrOptions?: string | { baseUrl?: string; apiKey?: string; timeout?: number }, options?: OntoDBOptions) {
    if (typeof urlOrOptions === 'object' && urlOrOptions !== null) {
      // Legacy TS-style: new OntoDBClient({ baseUrl, apiKey })
      super(urlOrOptions.baseUrl || 'http://localhost:7912', {
        apiKey: urlOrOptions.apiKey,
        timeout: urlOrOptions.timeout,
      });
    } else {
      // Canonical JS-style: new OntoDBClient(url, options)
      super(urlOrOptions || 'http://localhost:7912', options);
    }
  }
}

// Re-export everything from the JS SDK
export {
  OntoDB,
  OntoDBError,
  ConnectionError,
  QueryError,
  AuthenticationError,
  TimeoutError,
};

export type {
  OntoDBOptions,
  QueryResult,
  VectorSearchOptions,
  GraphTraverseOptions,
  HealthStatus,
  MetricsInfo,
  SchemaInfo,
  ApiResponse,
  Vertex,
  Edge,
  HealthResponse,
  BackupResult,
};

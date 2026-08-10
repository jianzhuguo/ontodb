/**
 * OntoDB JavaScript/TypeScript SDK
 *
 * @example
 * ```ts
 * import { OntoDB } from 'ontodb';
 *
 * const db = new OntoDB('http://localhost:7912', { apiKey: 'your-key' });
 *
 * // SQL query
 * const rows = await db.query('SELECT * FROM users LIMIT 10');
 *
 * // Vector search
 * const results = await db.vectorSearch('documents', 'embedding', [0.1, 0.2, ...], { topK: 5 });
 *
 * // SPARQL
 * const sparqlResults = await db.sparql('SELECT ?x WHERE { ?x rdf:type :Person }');
 * ```
 */

export { OntoDB } from './client';
export type {
  OntoDBOptions,
  QueryResult,
  VectorSearchOptions,
  GraphTraverseOptions,
  HealthStatus,
  MetricsInfo,
  SchemaInfo,
} from './types';
export {
  OntoDBError,
  ConnectionError,
  QueryError,
  AuthenticationError,
  TimeoutError,
} from './errors';

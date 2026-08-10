/** Base error for OntoDB SDK */
export class OntoDBError extends Error {
  constructor(message: string) {
    super(message);
    this.name = 'OntoDBError';
  }
}

/** Connection error */
export class ConnectionError extends OntoDBError {
  constructor(message: string) {
    super(message);
    this.name = 'ConnectionError';
  }
}

/** Query execution error */
export class QueryError extends OntoDBError {
  constructor(message: string) {
    super(message);
    this.name = 'QueryError';
  }
}

/** Authentication error */
export class AuthenticationError extends OntoDBError {
  constructor(message: string) {
    super(message);
    this.name = 'AuthenticationError';
  }
}

/** Timeout error */
export class TimeoutError extends OntoDBError {
  constructor(message: string) {
    super(message);
    this.name = 'TimeoutError';
  }
}

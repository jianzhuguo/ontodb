import { describe, it, expect } from 'vitest';
import { OntoDB } from '../src/client';
import { OntoDBError, QueryError, AuthenticationError, ConnectionError, TimeoutError } from '../src/errors';

describe('OntoDB', () => {
  describe('constructor', () => {
    it('should create client with default options', () => {
      const db = new OntoDB('http://localhost:7912');
      expect(db.toString()).toBe("OntoDB('http://localhost:7912')");
    });

    it('should strip trailing slash from URL', () => {
      const db = new OntoDB('http://localhost:7912/');
      expect(db.toString()).toBe("OntoDB('http://localhost:7912')");
    });

    it('should accept API key', () => {
      const db = new OntoDB('http://localhost:7912', { apiKey: 'test-key' });
      expect(db).toBeDefined();
    });
  });

  describe('isReady', () => {
    it('should return false when server is unreachable', async () => {
      const db = new OntoDB('http://127.0.0.1:1', { maxRetries: 0, timeout: 1000 });
      const ready = await db.isReady();
      expect(ready).toBe(false);
    });
  });
});

describe('Errors', () => {
  it('OntoDBError should be instance of Error', () => {
    const err = new OntoDBError('test');
    expect(err).toBeInstanceOf(Error);
    expect(err.name).toBe('OntoDBError');
  });

  it('ConnectionError should be instance of OntoDBError', () => {
    const err = new ConnectionError('test');
    expect(err).toBeInstanceOf(OntoDBError);
    expect(err.name).toBe('ConnectionError');
  });

  it('QueryError should be instance of OntoDBError', () => {
    const err = new QueryError('test');
    expect(err).toBeInstanceOf(OntoDBError);
    expect(err.name).toBe('QueryError');
  });

  it('AuthenticationError should be instance of OntoDBError', () => {
    const err = new AuthenticationError('test');
    expect(err).toBeInstanceOf(OntoDBError);
    expect(err.name).toBe('AuthenticationError');
  });

  it('TimeoutError should be instance of OntoDBError', () => {
    const err = new TimeoutError('test');
    expect(err).toBeInstanceOf(OntoDBError);
    expect(err.name).toBe('TimeoutError');
  });
});

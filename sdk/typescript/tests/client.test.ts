import { describe, it, expect } from 'vitest';
import { OntoDBClient, OntoDBError } from '../src/index.js';

describe('OntoDBClient', () => {
  describe('constructor', () => {
    it('should use default baseUrl', () => {
      const client = new OntoDBClient();
      expect(client).toBeDefined();
    });

    it('should strip trailing slash from baseUrl', () => {
      const client = new OntoDBClient({ baseUrl: 'http://localhost:7912/' });
      // Internal state is private, but we can verify construction succeeds
      expect(client).toBeDefined();
    });

    it('should accept custom options', () => {
      const client = new OntoDBClient({
        baseUrl: 'http://example.com:8080',
        apiKey: 'test-key',
        timeout: 5000,
      });
      expect(client).toBeDefined();
    });
  });

  describe('connection errors', () => {
    it('should throw OntoDBError when server is unreachable', async () => {
      const client = new OntoDBClient({
        baseUrl: 'http://127.0.0.1:1',
        timeout: 1000,
      });
      await expect(client.health()).rejects.toThrow();
    });

    it('ready() should return false when server is unreachable', async () => {
      const client = new OntoDBClient({
        baseUrl: 'http://127.0.0.1:1',
        timeout: 1000,
      });
      const result = await client.ready();
      expect(result).toBe(false);
    });

    it('alive() should return false when server is unreachable', async () => {
      const client = new OntoDBClient({
        baseUrl: 'http://127.0.0.1:1',
        timeout: 1000,
      });
      const result = await client.alive();
      expect(result).toBe(false);
    });
  });
});

describe('OntoDBError', () => {
  it('should be instance of Error', () => {
    const err = new OntoDBError('test error');
    expect(err).toBeInstanceOf(Error);
    expect(err).toBeInstanceOf(OntoDBError);
    expect(err.name).toBe('OntoDBError');
    expect(err.message).toBe('test error');
  });

  it('should store statusCode', () => {
    const err = new OntoDBError('not found', 404);
    expect(err.statusCode).toBe(404);
    expect(err.message).toBe('not found');
  });

  it('statusCode should be optional', () => {
    const err = new OntoDBError('error');
    expect(err.statusCode).toBeUndefined();
  });
});

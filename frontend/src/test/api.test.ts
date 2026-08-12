import { describe, it, expect, vi } from 'vitest';

// Mock fetch for API client tests
const mockFetch = vi.fn();
vi.stubGlobal('fetch', mockFetch);

describe('API Client', () => {
  it('should export apiClient with expected methods', async () => {
    const { apiClient } = await import('../api/client');
    expect(apiClient).toBeDefined();
    expect(typeof apiClient.getHealth).toBe('function');
    expect(typeof apiClient.getCluster).toBe('function');
    expect(typeof apiClient.getMetrics).toBe('function');
    expect(typeof apiClient.getSchema).toBe('function');
    expect(typeof apiClient.query).toBe('function');
  });

  it('should call fetch with correct path for getHealth', async () => {
    mockFetch.mockResolvedValueOnce({
      ok: true,
      json: () => Promise.resolve({ status: 'ok', version: '0.6.0' }),
    });

    const { apiClient } = await import('../api/client');
    await apiClient.getHealth();

    expect(mockFetch).toHaveBeenCalledWith('/api/health');
  });

  it('should call fetch with POST for query', async () => {
    mockFetch.mockResolvedValueOnce({
      ok: true,
      json: () => Promise.resolve({ data: [], elapsed_ms: 1 }),
    });

    const { apiClient } = await import('../api/client');
    await apiClient.query('SELECT 1');

    expect(mockFetch).toHaveBeenCalledWith('/api/query', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ query: 'SELECT 1' }),
    });
  });

  it('should throw on non-ok response', async () => {
    mockFetch.mockResolvedValueOnce({
      ok: false,
      status: 500,
    });

    const { apiClient } = await import('../api/client');
    await expect(apiClient.getHealth()).rejects.toThrow('API error: 500');
  });
});

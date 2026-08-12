import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import React from 'react';

// Mock fetch globally for component tests
const mockFetch = vi.fn();
vi.stubGlobal('fetch', mockFetch);

beforeEach(() => {
  vi.resetModules();
  mockFetch.mockReset();
  vi.stubGlobal('fetch', mockFetch);
});

// ============================================================
// Header Component
// ============================================================
describe('Header Component', () => {
  it('should render OntoDB brand name', async () => {
    const { Header } = await import('../components/Header');
    // Header reads from zustand store, render it
    render(React.createElement(Header));
    expect(screen.getByText('OntoDB')).toBeInTheDocument();
  });

  it('should display Disconnected when store has connected=false', async () => {
    const { Header } = await import('../components/Header');
    render(React.createElement(Header));
    expect(screen.getByText('Disconnected')).toBeInTheDocument();
  });

  it('should display version placeholder when no health data', async () => {
    const { Header } = await import('../components/Header');
    render(React.createElement(Header));
    // When health is null, version shows '?'
    expect(screen.getByText('v?')).toBeInTheDocument();
  });

  it('should display mode placeholder when no cluster data', async () => {
    const { Header } = await import('../components/Header');
    render(React.createElement(Header));
    // When cluster is null, mode shows '—'
    expect(screen.getByText('—')).toBeInTheDocument();
  });
});

// ============================================================
// MetricsPanel Component
// ============================================================
describe('MetricsPanel Component', () => {
  it('should show loading message when metrics is null', async () => {
    const { MetricsPanel } = await import('../components/MetricsPanel');
    render(React.createElement(MetricsPanel));
    expect(screen.getByText('Loading metrics...')).toBeInTheDocument();
  });
});

// ============================================================
// Dashboard Store
// ============================================================
describe('Dashboard Store', () => {
  it('should have correct initial state', async () => {
    const { useDashboardStore } = await import('../stores/dashboard');
    const state = useDashboardStore.getState();

    expect(state.health).toBeNull();
    expect(state.cluster).toBeNull();
    expect(state.metrics).toBeNull();
    expect(state.connected).toBe(false);
    expect(state.selectedNode).toBeNull();
    expect(state.lastUpdate).toBe(0);
    expect(state.metricsHistory).toEqual([]);
  });

  it('should update selectedNode via setSelectedNode', async () => {
    const { useDashboardStore } = await import('../stores/dashboard');
    const store = useDashboardStore.getState();

    store.setSelectedNode('node-1');
    expect(useDashboardStore.getState().selectedNode).toBe('node-1');

    store.setSelectedNode(null);
    expect(useDashboardStore.getState().selectedNode).toBeNull();
  });

  it('should set connected=false on fetchAll failure', async () => {
    mockFetch.mockRejectedValue(new Error('Network down'));

    const { useDashboardStore } = await import('../stores/dashboard');
    const store = useDashboardStore.getState();

    // fetchAll internally calls apiClient which calls fetch
    // apiClient.getHealth/getCluster/getMetrics all use fetch
    // Since fetch is mocked to reject, all three will fail
    // Promise.all will reject, so fetchAll catches and sets connected=false
    await store.fetchAll();

    expect(useDashboardStore.getState().connected).toBe(false);
  });

  it('should set connected=true on fetchAll success', async () => {
    const healthData = {
      status: 'ok', engine: 'ontodb', version: '0.6.0', uptime_seconds: 3600,
      checks: { query_engine: 'ok', storage: 'ok', storage_detail: { memtable_entries: 10, memtable_size_bytes: 1024, num_levels: 3, sst_size_bytes: 4096, total_sstables: 5 } },
    };
    const clusterData = { node_id: 'n1', mode: 'standalone' as const };
    const metricsData = {
      server: { uptime_seconds: 3600, version: '0.6.0' },
      queries: { total: 100, by_type: { select: 80, insert: 10, update: 5, delete: 3, vector_search: 1, hybrid: 1 }, errors: 0, avg_latency_ms: 2.0 },
      slow_queries: { total: 0, threshold_ms: 100 },
      storage: { sstable_count: 3, entries: 5000, compactions: 10, memtable_entries: 50, memtable_size_bytes: 2048 },
      connections: { http: { active: 5, total: 20 }, tcp: { active: 2, total: 10 }, pgwire: { active: 0, total: 0 } },
      auth: { successes: 50, failures: 0 },
      rate_limiting: { limited_total: 0 },
    };

    mockFetch
      .mockResolvedValueOnce({ ok: true, json: () => Promise.resolve(healthData) })
      .mockResolvedValueOnce({ ok: true, json: () => Promise.resolve(clusterData) })
      .mockResolvedValueOnce({ ok: true, json: () => Promise.resolve(metricsData) });

    const { useDashboardStore } = await import('../stores/dashboard');
    const store = useDashboardStore.getState();
    await store.fetchAll();

    const state = useDashboardStore.getState();
    expect(state.connected).toBe(true);
    expect(state.health).toEqual(healthData);
    expect(state.cluster).toEqual(clusterData);
    expect(state.metrics).toEqual(metricsData);
    expect(state.lastUpdate).toBeGreaterThan(0);
    expect(state.metricsHistory).toHaveLength(1);
  });

  it('should build topology nodes from cluster data', async () => {
    const healthData = {
      status: 'ok', engine: 'ontodb', version: '0.6.0', uptime_seconds: 100,
      checks: { query_engine: 'ok', storage: 'ok', storage_detail: { memtable_entries: 10, memtable_size_bytes: 1024, num_levels: 3, sst_size_bytes: 4096, total_sstables: 5 } },
    };
    const clusterData = { node_id: 'n1', mode: 'cluster' as const, leader: 'n1', peers: { n2: '127.0.0.1:7913', n3: '127.0.0.1:7914' } };
    const metricsData = {
      server: { uptime_seconds: 100, version: '0.6.0' },
      queries: { total: 50, by_type: { select: 40, insert: 5, update: 3, delete: 1, vector_search: 1, hybrid: 0 }, errors: 0, avg_latency_ms: 1.0 },
      slow_queries: { total: 0, threshold_ms: 100 },
      storage: { sstable_count: 2, entries: 2000, compactions: 3, memtable_entries: 20, memtable_size_bytes: 512 },
      connections: { http: { active: 3, total: 15 }, tcp: { active: 2, total: 8 }, pgwire: { active: 0, total: 0 } },
      auth: { successes: 30, failures: 0 },
      rate_limiting: { limited_total: 0 },
    };

    mockFetch
      .mockResolvedValueOnce({ ok: true, json: () => Promise.resolve(healthData) })
      .mockResolvedValueOnce({ ok: true, json: () => Promise.resolve(clusterData) })
      .mockResolvedValueOnce({ ok: true, json: () => Promise.resolve(metricsData) });

    const { useDashboardStore } = await import('../stores/dashboard');
    await useDashboardStore.getState().fetchAll();

    const state = useDashboardStore.getState();
    // 1 current node + 2 peers = 3 topology nodes
    expect(state.topologyNodes).toHaveLength(3);
    expect(state.topologyEdges).toHaveLength(2);
    // Current node should be leader
    expect(state.topologyNodes[0].role).toBe('leader');
    expect(state.topologyNodes[0].health).toBe('healthy');
  });

  it('should accumulate metricsHistory up to 60 entries', async () => {
    const healthData = {
      status: 'ok', engine: 'ontodb', version: '0.6.0', uptime_seconds: 10,
      checks: { query_engine: 'ok', storage: 'ok', storage_detail: { memtable_entries: 10, memtable_size_bytes: 1024, num_levels: 3, sst_size_bytes: 4096, total_sstables: 5 } },
    };
    const clusterData = { node_id: 'n1', mode: 'standalone' as const };
    const metricsData = {
      server: { uptime_seconds: 10, version: '0.6.0' },
      queries: { total: 10, by_type: { select: 10, insert: 0, update: 0, delete: 0, vector_search: 0, hybrid: 0 }, errors: 0, avg_latency_ms: 1.0 },
      slow_queries: { total: 0, threshold_ms: 100 },
      storage: { sstable_count: 1, entries: 100, compactions: 0, memtable_entries: 10, memtable_size_bytes: 256 },
      connections: { http: { active: 1, total: 1 }, tcp: { active: 0, total: 0 }, pgwire: { active: 0, total: 0 } },
      auth: { successes: 1, failures: 0 },
      rate_limiting: { limited_total: 0 },
    };

    // Reset and import fresh
    vi.resetModules();
    const freshMockFetch = vi.fn();
    vi.stubGlobal('fetch', freshMockFetch);

    const { useDashboardStore: freshStore } = await import('../stores/dashboard');

    // Pre-populate history with 59 entries to test the cap
    freshStore.setState({
      metricsHistory: Array.from({ length: 59 }, (_, i) => ({
        timestamp: Date.now() - (59 - i) * 3000,
        qps: i,
        latency: 1.0,
        connections: 1,
        storageEntries: 100,
      })),
    });

    freshMockFetch
      .mockResolvedValueOnce({ ok: true, json: () => Promise.resolve(healthData) })
      .mockResolvedValueOnce({ ok: true, json: () => Promise.resolve(clusterData) })
      .mockResolvedValueOnce({ ok: true, json: () => Promise.resolve(metricsData) });

    await freshStore.getState().fetchAll();

    const state = freshStore.getState();
    expect(state.metricsHistory).toHaveLength(60);
  });
});

// ============================================================
// VectorSearch Component
// ============================================================
describe('VectorSearch Component', () => {
  it('should render default table name and column', async () => {
    const { VectorSearch } = await import('../components/VectorSearch');
    render(React.createElement(VectorSearch));

    // Should show default inputs
    const tableInput = screen.getByDisplayValue('documents');
    const columnInput = screen.getByDisplayValue('embedding');
    expect(tableInput).toBeInTheDocument();
    expect(columnInput).toBeInTheDocument();
  });

  it('should render Top K input with default value 10', async () => {
    const { VectorSearch } = await import('../components/VectorSearch');
    render(React.createElement(VectorSearch));

    const topKInput = screen.getByDisplayValue('10');
    expect(topKInput).toBeInTheDocument();
  });

  it('should show placeholder text when no results', async () => {
    const { VectorSearch } = await import('../components/VectorSearch');
    render(React.createElement(VectorSearch));

    expect(screen.getByText('输入向量并点击搜索')).toBeInTheDocument();
  });

  it('should call fetch with correct body on search', async () => {
    mockFetch.mockResolvedValueOnce({
      ok: true,
      json: () => Promise.resolve({ data: [{ id: 1, _score: 0.95 }] }),
    });

    const { VectorSearch } = await import('../components/VectorSearch');
    render(React.createElement(VectorSearch));

    // Type a vector into the textarea
    const textarea = screen.getByPlaceholderText('0.1, 0.2, 0.3, ...');
    fireEvent.change(textarea, { target: { value: '0.1, 0.2, 0.3' } });

    // Click search button
    const searchButton = screen.getByText('向量搜索');
    fireEvent.click(searchButton);

    await waitFor(() => {
      expect(mockFetch).toHaveBeenCalledWith('/api/vector/search', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          class: 'documents',
          column: 'embedding',
          query_vector: [0.1, 0.2, 0.3],
          top_k: 10,
        }),
      });
    });
  });

  it('should display search results after successful search', async () => {
    mockFetch.mockResolvedValueOnce({
      ok: true,
      json: () => Promise.resolve({
        data: [
          { id: 'doc1', title: 'Test Doc', _score: 0.95 },
          { id: 'doc2', title: 'Another Doc', _score: 0.80 },
        ],
      }),
    });

    const { VectorSearch } = await import('../components/VectorSearch');
    render(React.createElement(VectorSearch));

    const textarea = screen.getByPlaceholderText('0.1, 0.2, 0.3, ...');
    fireEvent.change(textarea, { target: { value: '0.5, 0.5, 0.5' } });

    const searchButton = screen.getByText('向量搜索');
    fireEvent.click(searchButton);

    await waitFor(() => {
      expect(screen.getByText('找到 2 个结果')).toBeInTheDocument();
    });
  });

  it('should display error message on search failure', async () => {
    mockFetch.mockResolvedValueOnce({
      ok: true,
      json: () => Promise.resolve({ error: 'Table not found' }),
    });

    const { VectorSearch } = await import('../components/VectorSearch');
    render(React.createElement(VectorSearch));

    const textarea = screen.getByPlaceholderText('0.1, 0.2, 0.3, ...');
    fireEvent.change(textarea, { target: { value: '0.1, 0.2' } });

    const searchButton = screen.getByText('向量搜索');
    fireEvent.click(searchButton);

    await waitFor(() => {
      expect(screen.getByText('Table not found')).toBeInTheDocument();
    });
  });

  it('should display error on network failure', async () => {
    mockFetch.mockRejectedValueOnce(new Error('Connection refused'));

    const { VectorSearch } = await import('../components/VectorSearch');
    render(React.createElement(VectorSearch));

    const textarea = screen.getByPlaceholderText('0.1, 0.2, 0.3, ...');
    fireEvent.change(textarea, { target: { value: '0.1, 0.2' } });

    const searchButton = screen.getByText('向量搜索');
    fireEvent.click(searchButton);

    await waitFor(() => {
      expect(screen.getByText(/搜索失败/)).toBeInTheDocument();
    });
  });

  it('should include filter in request body when provided', async () => {
    mockFetch.mockResolvedValueOnce({
      ok: true,
      json: () => Promise.resolve({ data: [] }),
    });

    const { VectorSearch } = await import('../components/VectorSearch');
    render(React.createElement(VectorSearch));

    const textarea = screen.getByPlaceholderText('0.1, 0.2, 0.3, ...');
    fireEvent.change(textarea, { target: { value: '0.1' } });

    const filterInput = screen.getByPlaceholderText('category = "技术"');
    fireEvent.change(filterInput, { target: { value: 'type = "news"' } });

    const searchButton = screen.getByText('向量搜索');
    fireEvent.click(searchButton);

    await waitFor(() => {
      expect(mockFetch).toHaveBeenCalledWith('/api/vector/search', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          class: 'documents',
          column: 'embedding',
          query_vector: [0.1],
          top_k: 10,
          filter: 'type = "news"',
        }),
      });
    });
  });
});

// ============================================================
// QueryConsole Component
// ============================================================
describe('QueryConsole Component', () => {
  it('should render with default SQL text', async () => {
    const { QueryConsole } = await import('../components/QueryConsole');
    render(React.createElement(QueryConsole));

    const textarea = screen.getByPlaceholderText('输入 SQL 查询...');
    expect(textarea).toBeInTheDocument();
    expect(textarea).toHaveValue('SELECT * FROM users LIMIT 10');
  });

  it('should render execute and history buttons', async () => {
    const { QueryConsole } = await import('../components/QueryConsole');
    render(React.createElement(QueryConsole));

    expect(screen.getByText(/执行/)).toBeInTheDocument();
    expect(screen.getByText(/历史/)).toBeInTheDocument();
    expect(screen.getByText(/清空/)).toBeInTheDocument();
  });

  it('should show placeholder hint when no results', async () => {
    const { QueryConsole } = await import('../components/QueryConsole');
    render(React.createElement(QueryConsole));

    expect(screen.getByText('按 Ctrl+Enter 执行查询')).toBeInTheDocument();
    expect(screen.getByText('支持 SQL、SPARQL、GRAPH TRAVERSE')).toBeInTheDocument();
  });

  it('should execute query on button click and display results', async () => {
    mockFetch.mockResolvedValueOnce({
      ok: true,
      json: () => Promise.resolve({
        data: [{ id: 1, name: 'Alice' }, { id: 2, name: 'Bob' }],
        elapsed_ms: 3.5,
      }),
    });

    const { QueryConsole } = await import('../components/QueryConsole');
    render(React.createElement(QueryConsole));

    const executeButton = screen.getByText(/执行/);
    fireEvent.click(executeButton);

    await waitFor(() => {
      expect(mockFetch).toHaveBeenCalledWith('/api/query', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ query: 'SELECT * FROM users LIMIT 10' }),
      });
    });

    // Should display result row count
    await waitFor(() => {
      expect(screen.getByText(/2 行/)).toBeInTheDocument();
    });
  });

  it('should display error on query failure', async () => {
    mockFetch.mockResolvedValueOnce({
      ok: true,
      json: () => Promise.resolve({ error: 'Syntax error near FROM' }),
    });

    const { QueryConsole } = await import('../components/QueryConsole');
    render(React.createElement(QueryConsole));

    const executeButton = screen.getByText(/执行/);
    fireEvent.click(executeButton);

    await waitFor(() => {
      expect(screen.getByText(/Syntax error near FROM/)).toBeInTheDocument();
    });
  });

  it('should display error on network failure', async () => {
    mockFetch.mockRejectedValueOnce(new Error('Connection refused'));

    const { QueryConsole } = await import('../components/QueryConsole');
    render(React.createElement(QueryConsole));

    const executeButton = screen.getByText(/执行/);
    fireEvent.click(executeButton);

    await waitFor(() => {
      expect(screen.getByText(/Connection error/)).toBeInTheDocument();
    });
  });

  it('should allow SQL input changes', async () => {
    const { QueryConsole } = await import('../components/QueryConsole');
    render(React.createElement(QueryConsole));

    const textarea = screen.getByPlaceholderText('输入 SQL 查询...');
    fireEvent.change(textarea, { target: { value: 'SELECT count(*) FROM orders' } });

    expect(textarea).toHaveValue('SELECT count(*) FROM orders');
  });
});

export interface HealthResponse {
  status: string
  engine: string
  version: string
  uptime_seconds: number
  checks: {
    query_engine: string
    storage: string
    storage_detail: {
      memtable_entries: number
      memtable_size_bytes: number
      num_levels: number
      sst_size_bytes: number
      total_sstables: number
    }
  }
}

export interface ClusterNode {
  node_id: string
  mode: 'standalone' | 'cluster'
  leader?: string
  peers?: Record<string, string>
  replication_lag_ms?: number
}

export interface MetricsResponse {
  server: {
    uptime_seconds: number
    version: string
  }
  queries: {
    total: number
    by_type: {
      select: number
      insert: number
      update: number
      delete: number
      vector_search: number
      hybrid: number
    }
    errors: number
    avg_latency_ms: number
  }
  slow_queries: {
    total: number
    threshold_ms: number
  }
  storage: {
    sstable_count: number
    entries: number
    compactions: number
    memtable_entries: number
    memtable_size_bytes: number
  }
  connections: {
    http: { active: number; total: number }
    tcp: { active: number; total: number }
    pgwire: { active: number; total: number }
  }
  auth: {
    successes: number
    failures: number
  }
  rate_limiting: {
    limited_total: number
  }
}

export interface TopologyNode {
  id: string
  label: string
  role: 'leader' | 'follower' | 'standalone'
  health: 'healthy' | 'degraded' | 'unhealthy'
  cpu?: number
  memory?: number
  storage?: number
  connections?: number
  qps?: number
  position: [number, number, number]
}

export interface TopologyEdge {
  from: string
  to: string
  type: 'replication' | 'shard'
  lag_ms?: number
  throughput?: number
}

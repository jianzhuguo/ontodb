import type { HealthResponse, ClusterNode, MetricsResponse } from '../types'

const API_BASE = ''

async function api<T>(path: string): Promise<T> {
  const resp = await fetch(`${API_BASE}${path}`)
  if (!resp.ok) throw new Error(`API error: ${resp.status}`)
  return resp.json()
}

async function apiPost<T>(path: string, body: unknown): Promise<T> {
  const resp = await fetch(`${API_BASE}${path}`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  })
  if (!resp.ok) throw new Error(`API error: ${resp.status}`)
  return resp.json()
}

export const apiClient = {
  getHealth: () => api<HealthResponse>('/api/health'),
  getCluster: () => api<ClusterNode>('/api/cluster'),
  getMetrics: () => api<MetricsResponse>('/api/metrics'),
  getSchema: () => api<{ data: { classes: Array<{ name: string; columns: Array<{ name: string; data_type: string }> }> } }>('/api/schema'),
  query: (sql: string) => apiPost<{ data: unknown[]; elapsed_ms?: number }>('/api/query', { query: sql }),
}

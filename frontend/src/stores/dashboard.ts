import { create } from 'zustand'
import type { HealthResponse, ClusterNode, MetricsResponse, TopologyNode, TopologyEdge } from '../types'
import { apiClient } from '../api/client'

interface MetricsHistory {
  timestamp: number
  qps: number
  latency: number
  connections: number
  storageEntries: number
}

interface DashboardState {
  health: HealthResponse | null
  cluster: ClusterNode | null
  metrics: MetricsResponse | null
  metricsHistory: MetricsHistory[]
  topologyNodes: TopologyNode[]
  topologyEdges: TopologyEdge[]
  connected: boolean
  selectedNode: string | null
  lastUpdate: number

  setSelectedNode: (id: string | null) => void
  fetchAll: () => Promise<void>
}

function buildTopology(cluster: ClusterNode, health: HealthResponse, metrics: MetricsResponse): { nodes: TopologyNode[]; edges: TopologyEdge[] } {
  const nodes: TopologyNode[] = []
  const edges: TopologyEdge[] = []

  // Current node
  const currentHealth: TopologyNode['health'] =
    health.status === 'ok' ? 'healthy' : health.status === 'degraded' ? 'degraded' : 'unhealthy'

  nodes.push({
    id: cluster.node_id || 'standalone',
    label: `Node ${cluster.node_id || '1'}`,
    role: cluster.mode === 'standalone' ? 'standalone' : (cluster.leader === cluster.node_id ? 'leader' : 'follower'),
    health: currentHealth,
    memory: Math.round((metrics.storage.memtable_size_bytes / (1024 * 1024)) * 100) / 100,
    storage: metrics.storage.entries,
    connections: metrics.connections.http.active + metrics.connections.tcp.active,
    qps: metrics.queries.total,
    position: [0, 0, 0],
  })

  // Peer nodes
  if (cluster.peers) {
    const peerEntries = Object.entries(cluster.peers)
    const angleStep = (2 * Math.PI) / Math.max(peerEntries.length, 1)
    peerEntries.forEach(([id, _addr], i) => {
      const angle = angleStep * i
      const radius = 3
      nodes.push({
        id,
        label: `Node ${id}`,
        role: cluster.leader === id ? 'leader' : 'follower',
        health: 'healthy', // Assume healthy if connected
        position: [
          Math.cos(angle) * radius,
          0,
          Math.sin(angle) * radius,
        ],
      })
      edges.push({
        from: cluster.leader || cluster.node_id || '',
        to: id,
        type: 'replication',
        lag_ms: cluster.replication_lag_ms,
      })
    })
  }

  return { nodes, edges }
}

export const useDashboardStore = create<DashboardState>((set, get) => ({
  health: null,
  cluster: null,
  metrics: null,
  metricsHistory: [],
  topologyNodes: [],
  topologyEdges: [],
  connected: false,
  selectedNode: null,
  lastUpdate: 0,

  setSelectedNode: (id) => set({ selectedNode: id }),

  fetchAll: async () => {
    try {
      const [health, cluster, metrics] = await Promise.all([
        apiClient.getHealth(),
        apiClient.getCluster(),
        apiClient.getMetrics(),
      ])

      const { nodes, edges } = buildTopology(cluster, health, metrics)
      const now = Date.now()

      const history = get().metricsHistory
      const newEntry: MetricsHistory = {
        timestamp: now,
        qps: metrics.queries.total,
        latency: metrics.queries.avg_latency_ms,
        connections: metrics.connections.http.active + metrics.connections.tcp.active,
        storageEntries: metrics.storage.entries,
      }
      // Keep last 60 data points
      const newHistory = [...history, newEntry].slice(-60)

      set({
        health,
        cluster,
        metrics,
        metricsHistory: newHistory,
        topologyNodes: nodes,
        topologyEdges: edges,
        connected: true,
        lastUpdate: now,
      })
    } catch {
      set({ connected: false })
    }
  },
}))

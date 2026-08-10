import { useState, useEffect } from 'react'
import { MiniChart, Gauge } from './Charts'

interface MetricsData {
  queries_total: number
  queries_per_sec: number
  connections_active: number
  storage_entries: number
  memtable_size_mb: number
  disk_usage_mb: number
  cache_hit_rate: number
}

export function MetricsDashboard() {
  const [metrics, setMetrics] = useState<MetricsData | null>(null)
  const [qpsHistory, setQpsHistory] = useState<number[]>([])
  const [latencyHistory, setLatencyHistory] = useState<number[]>([])

  useEffect(() => {
    const fetchMetrics = async () => {
      try {
        const resp = await fetch('/api/metrics')
        const data = await resp.json()

        const newMetrics: MetricsData = {
          queries_total: data.queries?.total || 0,
          queries_per_sec: data.queries?.per_sec || 0,
          connections_active: (data.connections?.http?.active || 0) + (data.connections?.tcp?.active || 0),
          storage_entries: data.storage?.entries || 0,
          memtable_size_mb: Math.round((data.storage?.memtable_size_bytes || 0) / 1024 / 1024 * 100) / 100,
          disk_usage_mb: Math.round((data.storage?.disk_usage_bytes || 0) / 1024 / 1024 * 100) / 100,
          cache_hit_rate: data.cache?.hit_rate || 0,
        }

        setMetrics(newMetrics)
        setQpsHistory(prev => [...prev, newMetrics.queries_per_sec].slice(-30))
        setLatencyHistory(prev => [...prev, data.queries?.avg_latency_ms || 0].slice(-30))
      } catch (err) {
        console.error('Failed to fetch metrics:', err)
      }
    }

    fetchMetrics()
    const interval = setInterval(fetchMetrics, 2000)
    return () => clearInterval(interval)
  }, [])

  return (
    <div className="p-6 space-y-6">
      <div className="flex items-center justify-between">
        <h2 className="text-white text-lg font-semibold">实时指标</h2>
        <div className="flex items-center gap-2">
          <div className="w-2 h-2 rounded-full bg-green-500 animate-pulse" />
          <span className="text-xs text-gray-400">实时更新</span>
        </div>
      </div>

      {/* Stats grid */}
      <div className="grid grid-cols-2 lg:grid-cols-4 gap-4">
        <StatCard
          title="总查询数"
          value={metrics?.queries_total.toLocaleString() || '0'}
          icon="📊"
          color="blue"
          trend={qpsHistory}
        />
        <StatCard
          title="QPS"
          value={metrics?.queries_per_sec.toFixed(0) || '0'}
          icon="⚡"
          color="green"
          trend={qpsHistory}
        />
        <StatCard
          title="活跃连接"
          value={metrics?.connections_active.toString() || '0'}
          icon="🔗"
          color="purple"
        />
        <StatCard
          title="存储条目"
          value={metrics?.storage_entries.toLocaleString() || '0'}
          icon="💾"
          color="yellow"
        />
      </div>

      {/* Gauges */}
      <div className="grid grid-cols-3 gap-4">
        <div className="bg-gray-800 rounded-lg p-4 border border-gray-700 flex flex-col items-center">
          <Gauge
            value={metrics?.memtable_size_mb || 0}
            max={256}
            label="MemTable"
            unit="MB"
            color="#3b82f6"
            size={100}
          />
        </div>
        <div className="bg-gray-800 rounded-lg p-4 border border-gray-700 flex flex-col items-center">
          <Gauge
            value={metrics?.disk_usage_mb || 0}
            max={1000}
            label="磁盘使用"
            unit="MB"
            color="#22c55e"
            size={100}
          />
        </div>
        <div className="bg-gray-800 rounded-lg p-4 border border-gray-700 flex flex-col items-center">
          <Gauge
            value={Math.round((metrics?.cache_hit_rate || 0) * 100)}
            max={100}
            label="缓存命中"
            unit="%"
            color="#eab308"
            size={100}
          />
        </div>
      </div>

      {/* Charts */}
      <div className="grid grid-cols-2 gap-4">
        <div className="bg-gray-800 rounded-lg p-4 border border-gray-700">
          <h3 className="text-white text-sm font-medium mb-3">QPS 趋势</h3>
          <MiniChart data={qpsHistory} height={60} color="#22c55e" />
          <div className="flex justify-between mt-2 text-xs text-gray-500">
            <span>30s 前</span>
            <span className="text-green-400 font-medium">{metrics?.queries_per_sec.toFixed(0) || 0} QPS</span>
            <span>现在</span>
          </div>
        </div>
        <div className="bg-gray-800 rounded-lg p-4 border border-gray-700">
          <h3 className="text-white text-sm font-medium mb-3">延迟趋势</h3>
          <MiniChart data={latencyHistory} height={60} color="#eab308" />
          <div className="flex justify-between mt-2 text-xs text-gray-500">
            <span>30s 前</span>
            <span className="text-yellow-400 font-medium">{(latencyHistory[latencyHistory.length - 1] || 0).toFixed(1)}ms</span>
            <span>现在</span>
          </div>
        </div>
      </div>
    </div>
  )
}

function StatCard({ title, value, icon, color, trend }: {
  title: string
  value: string
  icon: string
  color: 'blue' | 'green' | 'purple' | 'yellow'
  trend?: number[]
}) {
  const colorClasses = {
    blue: 'bg-blue-900/30 border-blue-700 text-blue-400',
    green: 'bg-green-900/30 border-green-700 text-green-400',
    purple: 'bg-purple-900/30 border-purple-700 text-purple-400',
    yellow: 'bg-yellow-900/30 border-yellow-700 text-yellow-400',
  }

  return (
    <div className={`rounded-lg p-4 border ${colorClasses[color]}`}>
      <div className="flex items-center gap-2 mb-2">
        <span className="text-lg">{icon}</span>
        <span className="text-gray-400 text-xs">{title}</span>
      </div>
      <div className="text-2xl font-bold mb-2">{value}</div>
      {trend && trend.length > 1 && (
        <MiniChart data={trend} height={24} color={color === 'blue' ? '#3b82f6' : color === 'green' ? '#22c55e' : color === 'purple' ? '#a855f7' : '#eab308'} />
      )}
    </div>
  )
}

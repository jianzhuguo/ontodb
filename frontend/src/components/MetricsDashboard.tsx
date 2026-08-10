import { useState, useEffect } from 'react'

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
  const [history, setHistory] = useState<{ time: number; qps: number }[]>([])

  useEffect(() => {
    const fetchMetrics = async () => {
      try {
        const resp = await fetch('/api/metrics')
        const data = await resp.json()
        const now = Date.now()

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
        setHistory(prev => [...prev, { time: now, qps: newMetrics.queries_per_sec }].slice(-60))
      } catch (err) {
        console.error('Failed to fetch metrics:', err)
      }
    }

    fetchMetrics()
    const interval = setInterval(fetchMetrics, 2000)
    return () => clearInterval(interval)
  }, [])

  const maxQps = Math.max(...history.map(h => h.qps), 1)

  return (
    <div className="p-6 space-y-6">
      <h2 className="text-white text-lg font-semibold">实时指标</h2>

      {/* Stats grid */}
      <div className="grid grid-cols-2 lg:grid-cols-4 gap-4">
        <StatCard
          title="总查询数"
          value={metrics?.queries_total.toLocaleString() || '0'}
          icon="📊"
          color="blue"
        />
        <StatCard
          title="QPS"
          value={metrics?.queries_per_sec.toFixed(0) || '0'}
          icon="⚡"
          color="green"
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

      {/* Storage stats */}
      <div className="grid grid-cols-3 gap-4">
        <div className="bg-gray-800 rounded-lg p-4 border border-gray-700">
          <div className="text-gray-400 text-xs mb-1">MemTable</div>
          <div className="text-white text-xl font-bold">{metrics?.memtable_size_mb || 0} MB</div>
        </div>
        <div className="bg-gray-800 rounded-lg p-4 border border-gray-700">
          <div className="text-gray-400 text-xs mb-1">磁盘使用</div>
          <div className="text-white text-xl font-bold">{metrics?.disk_usage_mb || 0} MB</div>
        </div>
        <div className="bg-gray-800 rounded-lg p-4 border border-gray-700">
          <div className="text-gray-400 text-xs mb-1">缓存命中率</div>
          <div className="text-white text-xl font-bold">{((metrics?.cache_hit_rate || 0) * 100).toFixed(1)}%</div>
        </div>
      </div>

      {/* QPS Chart */}
      <div className="bg-gray-800 rounded-lg p-4 border border-gray-700">
        <h3 className="text-white text-sm font-medium mb-4">QPS 趋势</h3>
        <div className="h-40 flex items-end gap-1">
          {history.map((entry, i) => {
            const height = (entry.qps / maxQps) * 100
            return (
              <div
                key={i}
                className="flex-1 bg-blue-500 rounded-t transition-all duration-300"
                style={{ height: `${Math.max(height, 2)}%` }}
                title={`${entry.qps.toFixed(0)} QPS`}
              />
            )
          })}
        </div>
        <div className="flex justify-between mt-2 text-xs text-gray-500">
          <span>60s 前</span>
          <span>现在</span>
        </div>
      </div>
    </div>
  )
}

function StatCard({ title, value, icon, color }: {
  title: string
  value: string
  icon: string
  color: 'blue' | 'green' | 'purple' | 'yellow'
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
      <div className="text-2xl font-bold">{value}</div>
    </div>
  )
}

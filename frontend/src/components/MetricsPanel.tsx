import { useDashboardStore } from '../stores/dashboard'
import { LineChart, Line, XAxis, YAxis, ResponsiveContainer, Tooltip, AreaChart, Area } from 'recharts'

function MetricCard({ label, value, color, sub }: { label: string; value: string | number; color?: string; sub?: string }) {
  return (
    <div className="bg-onto-bg border border-onto-border rounded-lg p-3">
      <div className="text-[10px] uppercase tracking-wider text-onto-dim mb-1">{label}</div>
      <div className="text-xl font-bold" style={{ color: color || '#e2e8f0' }}>{value}</div>
      {sub && <div className="text-[11px] text-onto-dim mt-1">{sub}</div>}
    </div>
  )
}

function MiniChart({ data, dataKey, color, title }: { data: unknown[]; dataKey: string; color: string; title: string }) {
  return (
    <div className="bg-onto-bg border border-onto-border rounded-lg p-3">
      <div className="text-[10px] uppercase tracking-wider text-onto-dim mb-2">{title}</div>
      <div className="h-16">
        <ResponsiveContainer width="100%" height="100%">
          <AreaChart data={data}>
            <defs>
              <linearGradient id={`grad-${dataKey}`} x1="0" y1="0" x2="0" y2="1">
                <stop offset="0%" stopColor={color} stopOpacity={0.3} />
                <stop offset="100%" stopColor={color} stopOpacity={0} />
              </linearGradient>
            </defs>
            <Area
              type="monotone"
              dataKey={dataKey}
              stroke={color}
              fill={`url(#grad-${dataKey})`}
              strokeWidth={1.5}
              dot={false}
            />
            <Tooltip
              contentStyle={{ background: '#111827', border: '1px solid #1e293b', borderRadius: 8, fontSize: 11 }}
              labelStyle={{ color: '#94a3b8' }}
            />
          </AreaChart>
        </ResponsiveContainer>
      </div>
    </div>
  )
}

export function MetricsPanel() {
  const metrics = useDashboardStore((s) => s.metrics)
  const metricsHistory = useDashboardStore((s) => s.metricsHistory)

  if (!metrics) {
    return (
      <div className="p-4 text-center text-onto-dim text-sm">
        Loading metrics...
      </div>
    )
  }

  const uptime = metrics.server?.uptime_seconds ?? 0
  const uptimeStr =
    uptime < 60
      ? `${uptime}s`
      : uptime < 3600
        ? `${Math.floor(uptime / 60)}m`
        : `${Math.floor(uptime / 3600)}h ${Math.floor((uptime % 3600) / 60)}m`

  return (
    <div className="flex-1 p-4 space-y-3 overflow-y-auto">
      <h3 className="text-xs font-semibold uppercase tracking-wider text-onto-dim">实时指标</h3>

      {/* Charts */}
      <div className="space-y-3">
        <MiniChart data={metricsHistory} dataKey="qps" color="#3b82f6" title="QPS 趋势" />
        <MiniChart data={metricsHistory} dataKey="latency" color="#a855f7" title="延迟趋势 (ms)" />
        <MiniChart data={metricsHistory} dataKey="connections" color="#22c55e" title="连接数趋势" />
      </div>

      {/* Query Stats */}
      <h3 className="text-xs font-semibold uppercase tracking-wider text-onto-dim pt-2">查询统计</h3>
      <div className="grid grid-cols-2 gap-2">
        <MetricCard label="Total Queries" value={metrics.queries.total} color="#3b82f6" />
        <MetricCard label="Errors" value={metrics.queries.errors} color={metrics.queries.errors > 0 ? '#ef4444' : '#22c55e'} />
        <MetricCard label="SELECT" value={metrics.queries.by_type.select} />
        <MetricCard label="INSERT" value={metrics.queries.by_type.insert} />
        <MetricCard label="UPDATE" value={metrics.queries.by_type.update} />
        <MetricCard label="DELETE" value={metrics.queries.by_type.delete} />
        <MetricCard label="Vector Search" value={metrics.queries.by_type.vector_search} color="#a855f7" />
        <MetricCard label="Slow Queries" value={metrics.slow_queries.total} color={metrics.slow_queries.total > 0 ? '#eab308' : '#22c55e'} />
      </div>

      {/* Storage */}
      <h3 className="text-xs font-semibold uppercase tracking-wider text-onto-dim pt-2">存储引擎</h3>
      <div className="grid grid-cols-2 gap-2">
        <MetricCard label="SSTables" value={metrics.storage.sstable_count} />
        <MetricCard label="Entries" value={metrics.storage.entries.toLocaleString()} />
        <MetricCard label="Compactions" value={metrics.storage.compactions} />
        <MetricCard
          label="MemTable"
          value={`${metrics.storage.memtable_entries} entries`}
          sub={`${(metrics.storage.memtable_size_bytes / 1024 / 1024).toFixed(2)} MB`}
        />
      </div>

      {/* Connections */}
      <h3 className="text-xs font-semibold uppercase tracking-wider text-onto-dim pt-2">连接</h3>
      <div className="grid grid-cols-2 gap-2">
        <MetricCard
          label="HTTP"
          value={`${metrics.connections.http.active} active`}
          sub={`${metrics.connections.http.total} total`}
        />
        <MetricCard
          label="TCP"
          value={`${metrics.connections.tcp.active} active`}
          sub={`${metrics.connections.tcp.total} total`}
        />
        <MetricCard
          label="PG Wire"
          value={`${metrics.connections.pgwire.active} active`}
          sub={`${metrics.connections.pgwire.total} total`}
        />
        <MetricCard label="Auth Failures" value={metrics.auth.failures} color={metrics.auth.failures > 0 ? '#ef4444' : '#22c55e'} />
      </div>
    </div>
  )
}

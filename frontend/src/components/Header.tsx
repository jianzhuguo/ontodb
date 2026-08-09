import { useDashboardStore } from '../stores/dashboard'

export function Header() {
  const health = useDashboardStore((s) => s.health)
  const connected = useDashboardStore((s) => s.connected)
  const cluster = useDashboardStore((s) => s.cluster)
  const lastUpdate = useDashboardStore((s) => s.lastUpdate)

  const uptime = health?.uptime_seconds ?? 0
  const uptimeStr =
    uptime < 60
      ? `${uptime}s`
      : uptime < 3600
        ? `${Math.floor(uptime / 60)}m`
        : `${Math.floor(uptime / 3600)}h ${Math.floor((uptime % 3600) / 60)}m`

  return (
    <header className="h-12 flex items-center justify-between px-5 bg-onto-surface border-b border-onto-border shrink-0">
      <div className="flex items-center gap-3">
        <div className="flex items-center gap-2">
          <svg viewBox="0 0 32 32" className="w-6 h-6" fill="none">
            <circle cx="16" cy="16" r="14" stroke="#3b82f6" strokeWidth="2" fill="#0a0e1a" />
            <circle cx="16" cy="16" r="4" fill="#3b82f6" />
            <circle cx="16" cy="6" r="2" fill="#22c55e" />
            <circle cx="24" cy="20" r="2" fill="#22c55e" />
            <circle cx="8" cy="20" r="2" fill="#eab308" />
          </svg>
          <span className="text-base font-semibold tracking-tight">
            <span className="text-onto-accent">OntoDB</span>
            <span className="text-onto-dim text-xs ml-2 font-normal">Digital Twin</span>
          </span>
        </div>
      </div>

      <div className="flex items-center gap-5 text-xs">
        <div className="flex items-center gap-4 text-onto-dim">
          <span>Mode: <span className="text-onto-text">{cluster?.mode ?? '—'}</span></span>
          <span>Uptime: <span className="text-onto-text">{uptimeStr}</span></span>
          <span>v{health?.version ?? '?'}</span>
        </div>
        <div className="flex items-center gap-2">
          <div className={`w-2 h-2 rounded-full ${connected ? 'bg-onto-green' : 'bg-onto-red'}`} />
          <span className={connected ? 'text-onto-green' : 'text-onto-red'}>
            {connected ? 'Connected' : 'Disconnected'}
          </span>
        </div>
        {lastUpdate > 0 && (
          <span className="text-onto-dim/50">
            {new Date(lastUpdate).toLocaleTimeString()}
          </span>
        )}
      </div>
    </header>
  )
}

import { useState, useEffect } from 'react'

interface ConnectionStatus {
  url: string
  connected: boolean
  version: string
  uptime: number
  lastCheck: number
}

export function ConnectionManager({ serverUrl }: { serverUrl: string }) {
  const [status, setStatus] = useState<ConnectionStatus | null>(null)
  const [checking, setChecking] = useState(false)

  const checkConnection = async () => {
    setChecking(true)
    try {
      const resp = await fetch(`${serverUrl}/api/health`, { signal: AbortSignal.timeout(3000) })
      const data = await resp.json()
      setStatus({
        url: serverUrl,
        connected: data.status === 'ok',
        version: data.version || 'unknown',
        uptime: data.uptime_seconds || 0,
        lastCheck: Date.now(),
      })
    } catch {
      setStatus({
        url: serverUrl,
        connected: false,
        version: '',
        uptime: 0,
        lastCheck: Date.now(),
      })
    } finally {
      setChecking(false)
    }
  }

  useEffect(() => {
    checkConnection()
    const interval = setInterval(checkConnection, 10000)
    return () => clearInterval(interval)
  }, [serverUrl])

  if (!status) return null

  const uptimeStr = status.uptime > 0
    ? `${Math.floor(status.uptime / 3600)}h ${Math.floor((status.uptime % 3600) / 60)}m`
    : 'N/A'

  return (
    <div className="flex items-center gap-2 px-3 py-1.5 bg-gray-800 rounded-lg">
      <div className={`w-2 h-2 rounded-full ${status.connected ? 'bg-green-500' : 'bg-red-500 animate-pulse'}`} />
      <span className="text-xs text-gray-400">{status.url}</span>
      {status.connected && (
        <>
          <span className="text-xs text-gray-500">|</span>
          <span className="text-xs text-gray-400">v{status.version}</span>
          <span className="text-xs text-gray-500">|</span>
          <span className="text-xs text-gray-400">up {uptimeStr}</span>
        </>
      )}
      <button
        onClick={checkConnection}
        disabled={checking}
        className="ml-1 text-gray-500 hover:text-gray-300 transition-colors"
      >
        {checking ? '⏳' : '🔄'}
      </button>
    </div>
  )
}

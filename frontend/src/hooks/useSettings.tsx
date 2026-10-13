import { useState, useEffect } from 'react'

interface QueryHistoryItem {
  sql: string
  timestamp: number
  elapsed_ms: number
  rows: number
  error?: string
}

interface Settings {
  serverUrl: string
  apiKey: string
  autoRefresh: boolean
  refreshInterval: number
  maxRows: number
  theme: 'dark' | 'light'
}

const STORAGE_KEY = 'ontodb-settings'
const HISTORY_KEY = 'ontodb-query-history'

export function useSettings() {
  const [settings, setSettings] = useState<Settings>(() => {
    const saved = localStorage.getItem(STORAGE_KEY)
    if (saved) {
      try { return JSON.parse(saved) } catch {}
    }
    return {
      serverUrl: 'http://127.0.0.1:7912',
      apiKey: '',
      autoRefresh: true,
      refreshInterval: 3000,
      maxRows: 1000,
      theme: 'dark' as const,
    }
  })

  useEffect(() => {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(settings))
  }, [settings])

  return { settings, setSettings }
}

export function useQueryHistory() {
  const [history, setHistory] = useState<QueryHistoryItem[]>(() => {
    const saved = localStorage.getItem(HISTORY_KEY)
    if (saved) {
      try { return JSON.parse(saved) } catch {}
    }
    return []
  })

  useEffect(() => {
    localStorage.setItem(HISTORY_KEY, JSON.stringify(history.slice(0, 100)))
  }, [history])

  const addEntry = (entry: QueryHistoryItem) => {
    setHistory(prev => [entry, ...prev].slice(0, 100))
  }

  const clearHistory = () => {
    setHistory([])
  }

  return { history, addEntry, clearHistory }
}

export function SettingsPanel({ settings, onChange }: {
  settings: Settings
  onChange: (s: Settings) => void
}) {
  return (
    <div className="p-4 space-y-4">
      <h3 className="text-white font-medium">设置</h3>

      <div>
        <label className="block text-xs text-gray-400 mb-1">服务器地址</label>
        <input
          value={settings.serverUrl}
          onChange={(e) => onChange({ ...settings, serverUrl: e.target.value })}
          className="w-full bg-gray-800 text-white text-sm rounded px-3 py-1.5 border border-gray-700 outline-none focus:border-blue-500"
        />
      </div>

      <div>
        <label className="block text-xs text-gray-400 mb-1">API Key</label>
        <input
          type="password"
          value={settings.apiKey}
          onChange={(e) => onChange({ ...settings, apiKey: e.target.value })}
          placeholder="可选"
          className="w-full bg-gray-800 text-white text-sm rounded px-3 py-1.5 border border-gray-700 outline-none focus:border-blue-500"
        />
      </div>

      <div className="flex items-center justify-between">
        <label className="text-xs text-gray-400">自动刷新</label>
        <button
          onClick={() => onChange({ ...settings, autoRefresh: !settings.autoRefresh })}
          className={`w-10 h-5 rounded-full transition-colors ${
            settings.autoRefresh ? 'bg-blue-600' : 'bg-gray-700'
          }`}
        >
          <div className={`w-4 h-4 rounded-full bg-white transition-transform ${
            settings.autoRefresh ? 'translate-x-5' : 'translate-x-0.5'
          }`} />
        </button>
      </div>

      <div>
        <label className="block text-xs text-gray-400 mb-1">刷新间隔 (ms)</label>
        <input
          type="number"
          value={settings.refreshInterval}
          onChange={(e) => onChange({ ...settings, refreshInterval: parseInt(e.target.value) || 3000 })}
          min={1000}
          max={30000}
          className="w-full bg-gray-800 text-white text-sm rounded px-3 py-1.5 border border-gray-700 outline-none focus:border-blue-500"
        />
      </div>

      <div>
        <label className="block text-xs text-gray-400 mb-1">最大显示行数</label>
        <input
          type="number"
          value={settings.maxRows}
          onChange={(e) => onChange({ ...settings, maxRows: parseInt(e.target.value) || 1000 })}
          min={100}
          max={10000}
          className="w-full bg-gray-800 text-white text-sm rounded px-3 py-1.5 border border-gray-700 outline-none focus:border-blue-500"
        />
      </div>
    </div>
  )
}

export function QueryHistorySidebar({ history, onSelect, onClear }: {
  history: QueryHistoryItem[]
  onSelect: (sql: string) => void
  onClear: () => void
}) {
  return (
    <div className="flex flex-col h-full">
      <div className="flex items-center justify-between p-3 border-b border-gray-800">
        <h3 className="text-white text-sm font-medium">查询历史</h3>
        <button
          onClick={onClear}
          className="text-xs text-gray-400 hover:text-red-400"
        >
          清空
        </button>
      </div>
      <div className="flex-1 overflow-y-auto">
        {history.length === 0 ? (
          <div className="p-4 text-gray-500 text-sm text-center">暂无历史</div>
        ) : (
          history.map((item, i) => (
            <button
              key={i}
              onClick={() => onSelect(item.sql)}
              className="w-full text-left p-3 hover:bg-gray-800 border-b border-gray-800/50 transition-colors"
            >
              <div className="text-gray-300 text-xs font-mono truncate">{item.sql}</div>
              <div className="flex gap-2 mt-1 text-xs text-gray-500">
                <span>{item.rows} 行</span>
                <span>{item.elapsed_ms.toFixed(1)}ms</span>
                <span>{new Date(item.timestamp).toLocaleTimeString()}</span>
              </div>
            </button>
          ))
        )}
      </div>
    </div>
  )
}

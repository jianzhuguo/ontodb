import { useState, useEffect } from 'react'
import { useDashboardStore } from './stores/dashboard'
import { useSettings, SettingsPanel } from './hooks/useSettings'
import { Header } from './components/Header'
import { Topology3D } from './components/Topology3D'
import { MetricsPanel } from './components/MetricsPanel'
import { NodeDetail } from './components/NodeDetail'
import { QueryConsole } from './components/QueryConsole'
import { DataBrowser } from './components/DataBrowser'
import { VectorSearch } from './components/VectorSearch'
import { GraphExplorer } from './components/GraphExplorer'
import { SchemaBrowser } from './components/SchemaBrowser'
import { MetricsDashboard } from './components/MetricsDashboard'
import { ConnectionManager } from './components/ConnectionManager'

type Tab = 'topology' | 'query' | 'data' | 'vector' | 'graph' | 'schema' | 'metrics'

export default function App() {
  const fetchAll = useDashboardStore((s) => s.fetchAll)
  const connected = useDashboardStore((s) => s.connected)
  const [activeTab, setActiveTab] = useState<Tab>('topology')
  const [showSettings, setShowSettings] = useState(false)
  const { settings, setSettings } = useSettings()

  useEffect(() => {
    fetchAll()
    if (settings.autoRefresh) {
      const interval = setInterval(fetchAll, settings.refreshInterval)
      return () => clearInterval(interval)
    }
  }, [fetchAll, settings.autoRefresh, settings.refreshInterval])

  // Keyboard shortcuts
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.ctrlKey || e.metaKey) {
        switch (e.key) {
          case '1': setActiveTab('topology'); e.preventDefault(); break
          case '2': setActiveTab('query'); e.preventDefault(); break
          case '3': setActiveTab('data'); e.preventDefault(); break
          case '4': setActiveTab('schema'); e.preventDefault(); break
          case '5': setActiveTab('vector'); e.preventDefault(); break
          case '6': setActiveTab('graph'); e.preventDefault(); break
          case '7': setActiveTab('metrics'); e.preventDefault(); break
          case ',': setShowSettings(!showSettings); e.preventDefault(); break
        }
      }
    }
    window.addEventListener('keydown', handler)
    return () => window.removeEventListener('keydown', handler)
  }, [showSettings])

  const tabs: { id: Tab; label: string; icon: string; shortcut: string }[] = [
    { id: 'topology', label: '拓扑监控', icon: '🌐', shortcut: 'Ctrl+1' },
    { id: 'query', label: 'SQL 控制台', icon: '💻', shortcut: 'Ctrl+2' },
    { id: 'data', label: '数据浏览', icon: '📊', shortcut: 'Ctrl+3' },
    { id: 'schema', label: 'Schema', icon: '📋', shortcut: 'Ctrl+4' },
    { id: 'vector', label: '向量搜索', icon: '🔍', shortcut: 'Ctrl+5' },
    { id: 'graph', label: '图谱浏览器', icon: '🕸️', shortcut: 'Ctrl+6' },
    { id: 'metrics', label: '实时指标', icon: '📈', shortcut: 'Ctrl+7' },
  ]

  return (
    <div className="h-screen w-screen flex flex-col overflow-hidden bg-gray-950">
      <Header onSettingsClick={() => setShowSettings(!showSettings)} />

      {/* Tab Navigation */}
      <div className="flex items-center border-b border-gray-800 bg-gray-900 overflow-x-auto">
        {tabs.map((tab) => (
          <button
            key={tab.id}
            onClick={() => setActiveTab(tab.id)}
            title={tab.shortcut}
            className={`flex items-center gap-1.5 px-4 py-2.5 text-sm font-medium transition-colors border-b-2 whitespace-nowrap ${
              activeTab === tab.id
                ? 'text-blue-400 border-blue-500 bg-gray-800/50'
                : 'text-gray-400 border-transparent hover:text-gray-300 hover:bg-gray-800/30'
            }`}
          >
            <span>{tab.icon}</span>
            <span>{tab.label}</span>
          </button>
        ))}
        <div className="flex-1" />
        <div className="pr-3">
          <ConnectionManager serverUrl={settings.serverUrl} />
        </div>
      </div>

      {/* Content */}
      <div className="flex-1 overflow-hidden">
        {activeTab === 'topology' && (
          <div className="flex h-full">
            <div className="flex-1 relative">
              <Topology3D />
              {!connected && (
                <div className="absolute inset-0 flex items-center justify-center bg-gray-950/80">
                  <div className="text-center">
                    <div className="w-3 h-3 rounded-full bg-red-500 mx-auto mb-3 animate-pulse" />
                    <p className="text-gray-400 text-sm">连接 OntoDB 服务器中...</p>
                    <p className="text-gray-500 text-xs mt-1">{settings.serverUrl}</p>
                  </div>
                </div>
              )}
            </div>
            <div className="w-96 flex flex-col border-l border-gray-800 bg-gray-900 overflow-y-auto">
              <NodeDetail />
              <MetricsPanel />
            </div>
          </div>
        )}

        {activeTab === 'query' && <QueryConsole />}
        {activeTab === 'data' && <DataBrowser />}
        {activeTab === 'schema' && <SchemaBrowser />}
        {activeTab === 'vector' && <VectorSearch />}
        {activeTab === 'graph' && <GraphExplorer />}
        {activeTab === 'metrics' && <MetricsDashboard />}
      </div>

      {/* Settings Modal */}
      {showSettings && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
          <div className="bg-gray-900 rounded-lg border border-gray-700 w-96 max-h-[80vh] overflow-y-auto">
            <div className="flex items-center justify-between p-4 border-b border-gray-800">
              <h2 className="text-white font-medium">设置</h2>
              <button
                onClick={() => setShowSettings(false)}
                className="text-gray-400 hover:text-white"
              >
                ✕
              </button>
            </div>
            <SettingsPanel settings={settings} onChange={setSettings} />
            <div className="p-4 border-t border-gray-800">
              <button
                onClick={() => setShowSettings(false)}
                className="w-full px-4 py-2 bg-blue-600 hover:bg-blue-700 text-white text-sm rounded"
              >
                保存
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  )
}

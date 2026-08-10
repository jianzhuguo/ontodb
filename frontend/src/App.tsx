import { useState, useEffect } from 'react'
import { useDashboardStore } from './stores/dashboard'
import { Header } from './components/Header'
import { Topology3D } from './components/Topology3D'
import { MetricsPanel } from './components/MetricsPanel'
import { NodeDetail } from './components/NodeDetail'
import { QueryConsole } from './components/QueryConsole'
import { DataBrowser } from './components/DataBrowser'
import { VectorSearch } from './components/VectorSearch'

type Tab = 'topology' | 'query' | 'data' | 'vector'

export default function App() {
  const fetchAll = useDashboardStore((s) => s.fetchAll)
  const connected = useDashboardStore((s) => s.connected)
  const [activeTab, setActiveTab] = useState<Tab>('topology')

  useEffect(() => {
    fetchAll()
    const interval = setInterval(fetchAll, 3000)
    return () => clearInterval(interval)
  }, [fetchAll])

  const tabs: { id: Tab; label: string; icon: string }[] = [
    { id: 'topology', label: '拓扑监控', icon: '🌐' },
    { id: 'query', label: 'SQL 控制台', icon: '💻' },
    { id: 'data', label: '数据浏览', icon: '📊' },
    { id: 'vector', label: '向量搜索', icon: '🔍' },
  ]

  return (
    <div className="h-screen w-screen flex flex-col overflow-hidden bg-gray-950">
      <Header />

      {/* Tab Navigation */}
      <div className="flex items-center border-b border-gray-800 bg-gray-900">
        {tabs.map((tab) => (
          <button
            key={tab.id}
            onClick={() => setActiveTab(tab.id)}
            className={`flex items-center gap-1.5 px-4 py-2.5 text-sm font-medium transition-colors border-b-2 ${
              activeTab === tab.id
                ? 'text-blue-400 border-blue-500 bg-gray-800/50'
                : 'text-gray-400 border-transparent hover:text-gray-300 hover:bg-gray-800/30'
            }`}
          >
            <span>{tab.icon}</span>
            <span>{tab.label}</span>
          </button>
        ))}
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
                    <p className="text-gray-500 text-xs mt-1">http://127.0.0.1:7912</p>
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

        {activeTab === 'query' && (
          <div className="h-full bg-gray-950">
            <QueryConsole />
          </div>
        )}

        {activeTab === 'data' && (
          <div className="h-full bg-gray-950">
            <DataBrowser />
          </div>
        )}

        {activeTab === 'vector' && (
          <div className="h-full bg-gray-950">
            <VectorSearch />
          </div>
        )}
      </div>
    </div>
  )
}

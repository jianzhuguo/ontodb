import { useEffect } from 'react'
import { useDashboardStore } from './stores/dashboard'
import { Header } from './components/Header'
import { Topology3D } from './components/Topology3D'
import { MetricsPanel } from './components/MetricsPanel'
import { NodeDetail } from './components/NodeDetail'

export default function App() {
  const fetchAll = useDashboardStore((s) => s.fetchAll)
  const connected = useDashboardStore((s) => s.connected)

  useEffect(() => {
    fetchAll()
    const interval = setInterval(fetchAll, 3000)
    return () => clearInterval(interval)
  }, [fetchAll])

  return (
    <div className="h-screen w-screen flex flex-col overflow-hidden bg-onto-bg">
      <Header />
      <div className="flex-1 flex overflow-hidden">
        {/* 3D Topology - Left */}
        <div className="flex-1 relative">
          <Topology3D />
          {!connected && (
            <div className="absolute inset-0 flex items-center justify-center bg-onto-bg/80">
              <div className="text-center">
                <div className="w-3 h-3 rounded-full bg-onto-red mx-auto mb-3 animate-pulse" />
                <p className="text-onto-dim text-sm">连接 OntoDB 服务器中...</p>
                <p className="text-onto-dim/50 text-xs mt-1">http://127.0.0.1:7912</p>
              </div>
            </div>
          )}
        </div>
        {/* Right Panel */}
        <div className="w-96 flex flex-col border-l border-onto-border bg-onto-surface overflow-y-auto">
          <NodeDetail />
          <MetricsPanel />
        </div>
      </div>
    </div>
  )
}

import { useDashboardStore } from '../stores/dashboard'

export function NodeDetail() {
  const selectedNode = useDashboardStore((s) => s.selectedNode)
  const topologyNodes = useDashboardStore((s) => s.topologyNodes)

  if (!selectedNode) {
    return (
      <div className="p-4 border-b border-onto-border">
        <h3 className="text-xs font-semibold uppercase tracking-wider text-onto-dim mb-2">集群拓扑</h3>
        <div className="space-y-2">
          {topologyNodes.map((node) => (
            <div
              key={node.id}
              className="flex items-center gap-3 p-2 rounded-lg bg-onto-bg border border-onto-border hover:border-onto-accent/30 cursor-pointer transition-colors"
              onClick={() => useDashboardStore.getState().setSelectedNode(node.id)}
            >
              <div
                className="w-2.5 h-2.5 rounded-full"
                style={{
                  backgroundColor:
                    node.health === 'healthy' ? '#22c55e' : node.health === 'degraded' ? '#eab308' : '#ef4444',
                }}
              />
              <div className="flex-1 min-w-0">
                <div className="text-sm font-medium truncate">{node.label}</div>
                <div className="text-[11px] text-onto-dim">{node.role.toUpperCase()}</div>
              </div>
              {node.qps !== undefined && (
                <div className="text-right">
                  <div className="text-xs text-onto-accent">{node.qps}</div>
                  <div className="text-[10px] text-onto-dim">QPS</div>
                </div>
              )}
            </div>
          ))}
        </div>
      </div>
    )
  }

  const node = topologyNodes.find((n) => n.id === selectedNode)
  if (!node) return null

  return (
    <div className="p-4 border-b border-onto-border">
      <div className="flex items-center justify-between mb-3">
        <h3 className="text-xs font-semibold uppercase tracking-wider text-onto-dim">节点详情</h3>
        <button
          className="text-onto-dim hover:text-onto-text text-xs"
          onClick={() => useDashboardStore.getState().setSelectedNode(null)}
        >
          ← 返回列表
        </button>
      </div>

      <div className="bg-onto-bg border border-onto-border rounded-lg p-4 space-y-3">
        <div className="flex items-center gap-3">
          <div
            className="w-4 h-4 rounded-full"
            style={{
              backgroundColor:
                node.health === 'healthy' ? '#22c55e' : node.health === 'degraded' ? '#eab308' : '#ef4444',
            }}
          />
          <div>
            <div className="text-base font-semibold">{node.label}</div>
            <div className="text-xs text-onto-dim">{node.id}</div>
          </div>
        </div>

        <div className="grid grid-cols-2 gap-3 text-sm">
          <div>
            <div className="text-[10px] text-onto-dim uppercase">Role</div>
            <div className="font-medium">{node.role.toUpperCase()}</div>
          </div>
          <div>
            <div className="text-[10px] text-onto-dim uppercase">Health</div>
            <div className="font-medium" style={{
              color: node.health === 'healthy' ? '#22c55e' : node.health === 'degraded' ? '#eab308' : '#ef4444'
            }}>
              {node.health.toUpperCase()}
            </div>
          </div>
          {node.qps !== undefined && (
            <div>
              <div className="text-[10px] text-onto-dim uppercase">QPS</div>
              <div className="font-medium text-onto-accent">{node.qps}</div>
            </div>
          )}
          {node.connections !== undefined && (
            <div>
              <div className="text-[10px] text-onto-dim uppercase">Connections</div>
              <div className="font-medium">{node.connections}</div>
            </div>
          )}
          {node.storage !== undefined && (
            <div>
              <div className="text-[10px] text-onto-dim uppercase">Storage Entries</div>
              <div className="font-medium">{node.storage.toLocaleString()}</div>
            </div>
          )}
          {node.memory !== undefined && (
            <div>
              <div className="text-[10px] text-onto-dim uppercase">MemTable</div>
              <div className="font-medium">{node.memory} MB</div>
            </div>
          )}
        </div>
      </div>
    </div>
  )
}

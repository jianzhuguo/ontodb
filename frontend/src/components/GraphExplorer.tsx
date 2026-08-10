import { useState, useCallback, useRef, useEffect } from 'react'

interface GraphNode {
  id: string
  label: string
  properties: Record<string, unknown>
  x: number
  y: number
}

interface GraphEdge {
  id: string
  from: string
  to: string
  label: string
}

export function GraphExplorer() {
  const [nodes, setNodes] = useState<GraphNode[]>([])
  const [edges, setEdges] = useState<GraphEdge[]>([])
  const [startId, setStartId] = useState('Person::1')
  const [depth, setDepth] = useState(2)
  const [direction, setDirection] = useState<'out' | 'in' | 'both'>('out')
  const [selectedNode, setSelectedNode] = useState<GraphNode | null>(null)
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const canvasRef = useRef<HTMLCanvasElement>(null)
  const [dragNode, setDragNode] = useState<GraphNode | null>(null)
  const [offset, setOffset] = useState({ x: 0, y: 0 })

  // Layout nodes in a circle
  const layoutNodes = useCallback((nodeList: GraphNode[]): GraphNode[] => {
    const cx = 400
    const cy = 300
    const radius = Math.min(200, 50 + nodeList.length * 30)
    return nodeList.map((node, i) => ({
      ...node,
      x: cx + radius * Math.cos(2 * Math.PI * i / nodeList.length),
      y: cy + radius * Math.sin(2 * Math.PI * i / nodeList.length),
    }))
  }, [])

  // Execute graph traversal
  const handleTraverse = useCallback(async () => {
    setLoading(true)
    setError(null)

    try {
      const resp = await fetch('/api/graph/traverse', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          start: startId,
          direction,
          max_depth: depth,
        }),
      })

      const data = await resp.json()
      if (data.error) {
        setError(data.error)
      } else {
        const result = data.data || {}
        const rawNodes = (result.vertices || []).map((v: { id: string; label?: string; properties?: Record<string, unknown> }) => ({
          id: v.id,
          label: v.label || v.id.split('::').pop() || v.id,
          properties: v.properties || {},
          x: 0,
          y: 0,
        }))
        const rawEdges = (result.edges || []).map((e: { id: string; from: string; to: string; label?: string }) => ({
          id: e.id,
          from: e.from,
          to: e.to,
          label: e.label || '',
        }))

        setNodes(layoutNodes(rawNodes))
        setEdges(rawEdges)
      }
    } catch (err) {
      setError(`遍历失败: ${err}`)
    } finally {
      setLoading(false)
    }
  }, [startId, depth, direction, layoutNodes])

  // Draw canvas
  useEffect(() => {
    const canvas = canvasRef.current
    if (!canvas) return

    const ctx = canvas.getContext('2d')
    if (!ctx) return

    const dpr = window.devicePixelRatio || 1
    canvas.width = canvas.offsetWidth * dpr
    canvas.height = canvas.offsetHeight * dpr
    ctx.scale(dpr, dpr)

    const w = canvas.offsetWidth
    const h = canvas.offsetHeight

    // Clear
    ctx.fillStyle = '#030712'
    ctx.fillRect(0, 0, w, h)

    // Draw edges
    ctx.strokeStyle = '#374151'
    ctx.lineWidth = 1.5
    edges.forEach((edge) => {
      const fromNode = nodes.find(n => n.id === edge.from)
      const toNode = nodes.find(n => n.id === edge.to)
      if (fromNode && toNode) {
        ctx.beginPath()
        ctx.moveTo(fromNode.x, fromNode.y)
        ctx.lineTo(toNode.x, toNode.y)
        ctx.stroke()

        // Arrow
        const angle = Math.atan2(toNode.y - fromNode.y, toNode.x - fromNode.x)
        const midX = (fromNode.x + toNode.x) / 2
        const midY = (fromNode.y + toNode.y) / 2
        ctx.fillStyle = '#6b7280'
        ctx.beginPath()
        ctx.moveTo(midX + 8 * Math.cos(angle), midY + 8 * Math.sin(angle))
        ctx.lineTo(midX + 8 * Math.cos(angle + 2.5), midY + 8 * Math.sin(angle + 2.5))
        ctx.lineTo(midX + 8 * Math.cos(angle - 2.5), midY + 8 * Math.sin(angle - 2.5))
        ctx.closePath()
        ctx.fill()

        // Edge label
        if (edge.label) {
          ctx.fillStyle = '#9ca3af'
          ctx.font = '10px Inter'
          ctx.textAlign = 'center'
          ctx.fillText(edge.label, midX, midY - 8)
        }
      }
    })

    // Draw nodes
    nodes.forEach((node) => {
      const isSelected = selectedNode?.id === node.id
      const isStart = node.id === startId

      // Node circle
      ctx.beginPath()
      ctx.arc(node.x, node.y, 20, 0, 2 * Math.PI)
      ctx.fillStyle = isSelected ? '#3b82f6' : isStart ? '#10b981' : '#1f2937'
      ctx.fill()
      ctx.strokeStyle = isSelected ? '#60a5fa' : isStart ? '#34d399' : '#4b5563'
      ctx.lineWidth = 2
      ctx.stroke()

      // Node label
      ctx.fillStyle = '#e5e7eb'
      ctx.font = 'bold 11px Inter'
      ctx.textAlign = 'center'
      ctx.textBaseline = 'middle'
      ctx.fillText(node.label.slice(0, 6), node.x, node.y)
    })
  }, [nodes, edges, selectedNode, startId])

  // Mouse interaction
  const handleMouseDown = (e: React.MouseEvent) => {
    const rect = canvasRef.current?.getBoundingClientRect()
    if (!rect) return
    const x = e.clientX - rect.left
    const y = e.clientY - rect.top

    const clicked = nodes.find(n => Math.hypot(n.x - x, n.y - y) < 20)
    if (clicked) {
      setDragNode(clicked)
      setOffset({ x: x - clicked.x, y: y - clicked.y })
      setSelectedNode(clicked)
    }
  }

  const handleMouseMove = (e: React.MouseEvent) => {
    if (!dragNode) return
    const rect = canvasRef.current?.getBoundingClientRect()
    if (!rect) return
    const x = e.clientX - rect.left - offset.x
    const y = e.clientY - rect.top - offset.y

    setNodes(prev => prev.map(n => n.id === dragNode.id ? { ...n, x, y } : n))
  }

  const handleMouseUp = () => {
    setDragNode(null)
  }

  return (
    <div className="flex h-full">
      {/* Controls */}
      <div className="w-64 border-r border-gray-800 bg-gray-900 p-4 space-y-4">
        <h3 className="text-white font-medium">图谱浏览器</h3>

        <div>
          <label className="block text-xs text-gray-400 mb-1">起始节点</label>
          <input
            value={startId}
            onChange={(e) => setStartId(e.target.value)}
            className="w-full bg-gray-800 text-white text-sm rounded px-3 py-1.5 border border-gray-700 outline-none focus:border-blue-500"
            placeholder="Person::1"
          />
        </div>

        <div>
          <label className="block text-xs text-gray-400 mb-1">遍历深度</label>
          <input
            type="number"
            value={depth}
            onChange={(e) => setDepth(parseInt(e.target.value) || 2)}
            min={1}
            max={10}
            className="w-full bg-gray-800 text-white text-sm rounded px-3 py-1.5 border border-gray-700 outline-none focus:border-blue-500"
          />
        </div>

        <div>
          <label className="block text-xs text-gray-400 mb-1">方向</label>
          <select
            value={direction}
            onChange={(e) => setDirection(e.target.value as 'out' | 'in' | 'both')}
            className="w-full bg-gray-800 text-white text-sm rounded px-3 py-1.5 border border-gray-700 outline-none focus:border-blue-500"
          >
            <option value="out">出边</option>
            <option value="in">入边</option>
            <option value="both">双向</option>
          </select>
        </div>

        <button
          onClick={handleTraverse}
          disabled={loading}
          className="w-full px-4 py-2 bg-blue-600 hover:bg-blue-700 disabled:bg-gray-600 text-white text-sm rounded font-medium transition-colors"
        >
          {loading ? '遍历中...' : '开始遍历'}
        </button>

        {error && (
          <div className="p-2 bg-red-900/30 border border-red-700 rounded text-red-400 text-xs">
            {error}
          </div>
        )}

        <div className="text-xs text-gray-500">
          节点: {nodes.length} · 边: {edges.length}
        </div>

        {/* Selected node details */}
        {selectedNode && (
          <div className="mt-4 p-3 bg-gray-800 rounded border border-gray-700">
            <h4 className="text-white text-sm font-medium mb-2">{selectedNode.label}</h4>
            <div className="space-y-1">
              <div className="text-xs text-gray-400">ID: {selectedNode.id}</div>
              {Object.entries(selectedNode.properties).map(([k, v]) => (
                <div key={k} className="text-xs">
                  <span className="text-gray-400">{k}: </span>
                  <span className="text-gray-200">{String(v)}</span>
                </div>
              ))}
            </div>
          </div>
        )}
      </div>

      {/* Canvas */}
      <canvas
        ref={canvasRef}
        className="flex-1 cursor-move"
        onMouseDown={handleMouseDown}
        onMouseMove={handleMouseMove}
        onMouseUp={handleMouseUp}
        onMouseLeave={handleMouseUp}
      />
    </div>
  )
}

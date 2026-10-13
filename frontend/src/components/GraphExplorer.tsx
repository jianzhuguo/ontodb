import { useState, useCallback, useRef, useEffect } from 'react'
import {
  forceSimulation,
  forceLink,
  forceManyBody,
  forceCenter,
  forceCollide,
  type SimulationNodeDatum,
  type SimulationLinkDatum,
} from 'd3-force'

// --- Entity type color map (sembio biomedical ontology) ---
const ENTITY_COLORS: Record<string, string> = {
  Drug: '#3b82f6',
  Disease: '#ef4444',
  Gene: '#22c55e',
  Protein: '#a855f7',
  Target: '#f97316',
  Pathway: '#eab308',
  MiRNA: '#06b6d4',
  Enzyme: '#ec4899',
  Kinase: '#8b5cf6',
  Cancer: '#dc2626',
  Metabolite: '#14b8a6',
  Lipid: '#f59e0b',
  CellLine: '#6366f1',
  BioAssay: '#84cc16',
  ClinicalTrial: '#10b981',
  Tissue: '#d946ef',
  ProteinClass: '#7c3aed',
  GeneOntology: '#059669',
  ATCClass: '#2563eb',
  MoleculeATC: '#2563eb',
  Variant: '#e11d48',
  SignalingPathway: '#ca8a04',
  MetabolicPathway: '#0d9488',
  ReceptorTyrosineKinase: '#7c3aed',
  TranscriptionFactor: '#9333ea',
  BindingSite: '#c026d3',
  StructuralDomain: '#6d28d9',
  StructuralProtein: '#4f46e5',
  ProteinStructure: '#4338ca',
  SubcellularLocation: '#1e40af',
  CompoundProperty: '#1d4ed8',
  Formulation: '#1e3a8a',
  DrugWarning: '#b91c1c',
  DrugIndication: '#dc2626',
  DrugMechanism: '#e11d48',
  DrugMetabolism: '#be123c',
  Biotherapeutic: '#059669',
  ProductPatent: '#047857',
  DailyDose: '#065f46',
  ChembProduct: '#064e3b',
  ChembVariant: '#0f766e',
  USANStem: '#115e59',
  MechanismRef: '#134e4a',
  IndicationRef: '#1e3a38',
  WarningRef: '#4a1d1d',
  Literature: '#78350f',
  ComponentGO: '#713f12',
  AssayType: '#854d0e',
  TissueExpression: '#a16207',
}

const DEFAULT_COLOR = '#6b7280'

// Relationship tables in sembio ontology
const RELATIONSHIP_TABLES = [
  'treats', 'targets', 'interacts_with', 'causes', 'catalyzes',
  'regulates', 'associated_with', 'metabolizes', 'participates_in',
  'member_of', 'has_structure', 'has_bioactivity', 'annotated_with',
  'mirna_disease', 'disease_target', 'disease_phenotype',
  'metabolite_disease', 'structure_drug', 'structure_disease',
]

function getEntityType(id: string): string {
  const parts = id.split('::')
  if (parts.length >= 3) return parts[1]
  if (parts.length >= 2) return parts[0].replace(/^[^.]+\./, '')
  return 'Unknown'
}

function getEntityColor(id: string): string {
  return ENTITY_COLORS[getEntityType(id)] || DEFAULT_COLOR
}

function getDisplayLabel(props: Record<string, unknown>, id: string): string {
  const name = (props.drug_name as string)
    || (props.disease_name as string)
    || (props.gene_name as string)
    || (props.name as string)
    || (props.label as string)
  if (name && name.length > 0 && name.length < 200) return name
  const parts = id.split('::')
  return parts[parts.length - 1] || id
}

// --- Types ---
interface SimNode extends SimulationNodeDatum {
  id: string
  label: string
  properties: Record<string, unknown>
  entityType: string
  color: string
  displayLabel: string
  radius: number
}

interface SimLink extends SimulationLinkDatum<SimNode> {
  label: string
}

interface SearchResult {
  id: string
  name: string
  type: string
}

// --- SQL query helper ---
async function sqlQuery<T>(query: string): Promise<T[]> {
  const resp = await fetch('/api/query', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ query }),
  })
  const data = await resp.json()
  if (!data.success) throw new Error(data.error || 'Query failed')
  return data.data || []
}

// Relationship type colors
const REL_COLORS: Record<string, string> = {
  treats: '#22c55e',
  targets: '#f97316',
  interacts_with: '#3b82f6',
  causes: '#ef4444',
  catalyzes: '#a855f7',
  regulates: '#06b6d4',
  associated_with: '#eab308',
  metabolizes: '#ec4899',
  participates_in: '#14b8a6',
  has_bioactivity: '#8b5cf6',
  mirna_disease: '#f59e0b',
  disease_target: '#dc2626',
  metabolized_by: '#d946ef',
  phosphorylates: '#6366f1',
  is_a: '#84cc16',
  metabolite_disease: '#0ea5e9',
  models: '#a3e635',
  evaluates: '#fb923c',
}
const DEFAULT_REL_COLOR = '#6b7280'

// --- Component ---
export function GraphExplorer() {
  const [selectedNode, setSelectedNode] = useState<SimNode | null>(null)
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [nodeCount, setNodeCount] = useState(0)
  const [edgeCount, setEdgeCount] = useState(0)
  const [searchQuery, setSearchQuery] = useState('')
  const [searchResults, setSearchResults] = useState<SearchResult[]>([])
  const [searching, setSearching] = useState(false)
  const [depth, setDepth] = useState(1)
  const [maxNeighbors, setMaxNeighbors] = useState(10)
  const [showEdgeLabels, setShowEdgeLabels] = useState(true)
  const [showInferred, setShowInferred] = useState(true)
  const [hiddenTypes, setHiddenTypes] = useState<Set<string>>(new Set())
  const [hiddenRels, setHiddenRels] = useState<Set<string>>(new Set())
  const [pathMode, setPathMode] = useState(false)
  const [pathStart, setPathStart] = useState<string | null>(null)
  const [pathEnd, setPathEnd] = useState<string | null>(null)
  const [highlightedPath, setHighlightedPath] = useState<Set<string>>(new Set())

  const canvasRef = useRef<HTMLCanvasElement>(null)
  const simRef = useRef<ReturnType<typeof forceSimulation> | null>(null)
  const nodesRef = useRef<SimNode[]>([])
  const linksRef = useRef<SimLink[]>([])
  const transformRef = useRef({ x: 0, y: 0, k: 1 })
  const dragRef = useRef<{ node: SimNode | null; offsetX: number; offsetY: number }>({ node: null, offsetX: 0, offsetY: 0 })
  const panRef = useRef<{ active: boolean; startX: number; startY: number }>({ active: false, startX: 0, startY: 0 })
  const rafRef = useRef<number>(0)

  // --- Search entities by name ---
  const handleSearch = useCallback(async () => {
    if (!searchQuery.trim()) return
    setSearching(true)
    setSearchResults([])
    try {
      const q = searchQuery.replace(/'/g, "''")
      // Search across multiple entity tables
      const results: SearchResult[] = []
      const searches = [
        { sql: `SELECT id, drug_name as name FROM sembio.Drug WHERE drug_name LIKE '%${q}%' LIMIT 10`, type: 'Drug' },
        { sql: `SELECT id, disease_name as name FROM sembio.Disease WHERE disease_name LIKE '%${q}%' LIMIT 10`, type: 'Disease' },
        { sql: `SELECT id, gene_name as name FROM sembio.Gene WHERE gene_name LIKE '%${q}%' LIMIT 10`, type: 'Gene' },
        { sql: `SELECT id, name FROM sembio.Protein WHERE name LIKE '%${q}%' LIMIT 10`, type: 'Protein' },
        { sql: `SELECT id, name FROM sembio.MiRNA WHERE name LIKE '%${q}%' LIMIT 10`, type: 'MiRNA' },
        { sql: `SELECT id, name FROM sembio.Enzyme WHERE name LIKE '%${q}%' LIMIT 5`, type: 'Enzyme' },
        { sql: `SELECT id, name FROM sembio.Kinase WHERE name LIKE '%${q}%' LIMIT 5`, type: 'Kinase' },
        { sql: `SELECT id, name FROM sembio.Target WHERE name LIKE '%${q}%' LIMIT 5`, type: 'Target' },
      ]

      const allResults = await Promise.allSettled(
        searches.map(s => sqlQuery<Record<string, string>>(s.sql))
      )

      allResults.forEach((result, i) => {
        if (result.status === 'fulfilled') {
          result.value.forEach((r) => {
            if (r.id && r.name) {
              results.push({ id: r.id, name: r.name, type: searches[i].type })
            }
          })
        }
      })

      setSearchResults(results.slice(0, 30))
    } catch {
      setSearchResults([])
    } finally {
      setSearching(false)
    }
  }, [searchQuery])

  // --- Traverse from an entity using SQL ---
  const handleTraverse = useCallback(async (startNodeId: string) => {
    if (!startNodeId.trim()) return
    setLoading(true)
    setError(null)

    try {
      const allNodes = new Map<string, { id: string; properties: Record<string, unknown> }>()
      const allEdges: Array<{ from: string; to: string; label: string }> = []
      const visited = new Set<string>()
      let frontier = [startNodeId]

      for (let d = 0; d < depth; d++) {
        const nextFrontier: string[] = []

        for (const entityId of frontier) {
          if (visited.has(entityId)) continue
          visited.add(entityId)

          const escapedId = entityId.replace(/'/g, "''")

          // Query all relationship tables for this entity
          const relQueries = RELATIONSHIP_TABLES.map(table => ({
            table,
            sql: `SELECT source, target FROM sembio.${table} WHERE source = '${escapedId}' OR target = '${escapedId}' LIMIT ${maxNeighbors}`,
          }))

          const relResults = await Promise.allSettled(
            relQueries.map(r => sqlQuery<Record<string, string>>(r.sql))
          )

          relResults.forEach((result, i) => {
            if (result.status === 'fulfilled') {
              result.value.forEach((row) => {
                const from = row.source
                const to = row.target
                if (!from || !to) return

                allEdges.push({ from, to, label: relQueries[i].table })

                const neighbor = from === entityId ? to : from
                if (!visited.has(neighbor)) {
                  nextFrontier.push(neighbor)
                }
              })
            }
          })
        }

        frontier = nextFrontier
      }

      // Collect all unique entity IDs
      const entityIds = new Set<string>()
      entityIds.add(startNodeId)
      allEdges.forEach(e => { entityIds.add(e.from); entityIds.add(e.to) })

      // Fetch entity details in batches by type
      const typeGroups = new Map<string, string[]>()
      entityIds.forEach(id => {
        const type = getEntityType(id)
        if (!typeGroups.has(type)) typeGroups.set(type, [])
        typeGroups.get(type)!.push(id)
      })

      for (const [type, ids] of typeGroups) {
        if (ids.length === 0) continue
        // Query the entity table for details
        const idList = ids.map(id => `'${id.replace(/'/g, "''")}'`).join(',')
        try {
          // Try to get entities with all columns
          const rows = await sqlQuery<Record<string, unknown>>(`SELECT * FROM sembio.${type} WHERE id IN (${idList}) LIMIT ${ids.length}`)
          rows.forEach(row => {
            const id = row.id as string
            if (id) {
              allNodes.set(id, { id, properties: row })
            }
          })
        } catch {
          // If table doesn't exist or query fails, create minimal nodes
          ids.forEach(id => {
            if (!allNodes.has(id)) {
              allNodes.set(id, { id, properties: {} })
            }
          })
        }
      }

      // Ensure all referenced entities have nodes
      entityIds.forEach(id => {
        if (!allNodes.has(id)) {
          allNodes.set(id, { id, properties: {} })
        }
      })

      buildGraph(
        Array.from(allNodes.values()),
        allEdges
      )
    } catch (err) {
      setError(`遍历失败: ${err}`)
    } finally {
      setLoading(false)
    }
  }, [depth, maxNeighbors])

  // --- Build force-directed graph ---
  const buildGraph = useCallback((rawNodes: Array<{ id: string; properties: Record<string, unknown> }>, rawEdges: Array<{ from: string; to: string; label: string }>) => {
    const canvas = canvasRef.current
    if (!canvas) return

    const w = canvas.offsetWidth
    const h = canvas.offsetHeight

    const nodeMap = new Map<string, SimNode>()
    const simNodes: SimNode[] = rawNodes.map((n) => {
      const entityType = getEntityType(n.id)
      const node: SimNode = {
        id: n.id,
        label: n.id.split('::').pop() || n.id,
        properties: n.properties,
        entityType,
        color: ENTITY_COLORS[entityType] || DEFAULT_COLOR,
        displayLabel: getDisplayLabel(n.properties, n.id),
        radius: 8,
        x: w / 2 + (Math.random() - 0.5) * 300,
        y: h / 2 + (Math.random() - 0.5) * 300,
      }
      nodeMap.set(n.id, node)
      return node
    })

    const simLinks: SimLink[] = rawEdges
      .filter((e) => nodeMap.has(e.from) && nodeMap.has(e.to) && e.from !== e.to)
      .map((e) => ({
        source: e.from,
        target: e.to,
        label: e.label,
      }))

    // Degree-based sizing
    const degree = new Map<string, number>()
    simLinks.forEach((l) => {
      const sid = typeof l.source === 'string' ? l.source : (l.source as SimNode).id
      const tid = typeof l.target === 'string' ? l.target : (l.target as SimNode).id
      degree.set(sid, (degree.get(sid) || 0) + 1)
      degree.set(tid, (degree.get(tid) || 0) + 1)
    })
    simNodes.forEach((n) => {
      const d = degree.get(n.id) || 0
      n.radius = Math.max(6, Math.min(24, 6 + d * 2))
    })

    nodesRef.current = simNodes
    linksRef.current = simLinks
    setNodeCount(simNodes.length)
    setEdgeCount(simLinks.length)

    if (simRef.current) simRef.current.stop()

    const sim = forceSimulation(simNodes)
      .force('link', forceLink<SimNode, SimLink>(simLinks).id((d) => d.id).distance(100).strength(0.3))
      .force('charge', forceManyBody().strength(-300))
      .force('center', forceCenter(w / 2, h / 2).strength(0.05))
      .force('collide', forceCollide<SimNode>().radius((d) => d.radius + 6).strength(0.7))
      .alpha(1)
      .alphaDecay(0.02)

    sim.on('tick', () => scheduleDraw())
    simRef.current = sim
    transformRef.current = { x: 0, y: 0, k: 1 }
  }, [])

  // --- Draw canvas ---
  const scheduleDraw = useCallback(() => {
    if (rafRef.current) cancelAnimationFrame(rafRef.current)
    rafRef.current = requestAnimationFrame(drawCanvas)
  }, [])

  const drawCanvas = useCallback(() => {
    const canvas = canvasRef.current
    if (!canvas) return
    const ctx = canvas.getContext('2d')
    if (!ctx) return

    const dpr = window.devicePixelRatio || 1
    const w = canvas.offsetWidth
    const h = canvas.offsetHeight
    canvas.width = w * dpr
    canvas.height = h * dpr
    ctx.scale(dpr, dpr)

    const { x: tx, y: ty, k } = transformRef.current

    ctx.fillStyle = '#030712'
    ctx.fillRect(0, 0, w, h)

    const nodes = nodesRef.current
    const links = linksRef.current

    // Empty state: show instructions
    if (nodes.length === 0) {
      ctx.fillStyle = '#374151'
      ctx.font = '16px Inter, sans-serif'
      ctx.textAlign = 'center'
      ctx.textBaseline = 'middle'
      ctx.fillText('🕸️ 知识图谱浏览器', w / 2, h / 2 - 40)
      ctx.font = '13px Inter, sans-serif'
      ctx.fillStyle = '#4b5563'
      ctx.fillText('在左侧搜索框输入实体名称（如药物、疾病、基因）', w / 2, h / 2)
      ctx.fillText('点击搜索结果即可展开关联图谱', w / 2, h / 2 + 24)
      ctx.fillStyle = '#374151'
      ctx.font = '11px Inter, sans-serif'
      ctx.fillText('或点击下方按钮加载示例数据', w / 2, h / 2 + 56)
      ctx.restore()
      return
    }

    ctx.save()
    ctx.translate(tx, ty)
    ctx.scale(k, k)

    // Draw edges
    links.forEach((link) => {
      const source = link.source as SimNode
      const target = link.target as SimNode
      if (!source.x || !source.y || !target.x || !target.y) return
      if (hiddenTypes.has(source.entityType) || hiddenTypes.has(target.entityType)) return

      // Determine edge style by relationship type
      const relType = link.label.split(' ')[0] // e.g. "treats (149,744)" → "treats"
      const isInferred = relType.startsWith('inferred_')
      const edgeColor = REL_COLORS[relType] || DEFAULT_REL_COLOR

      if (isInferred && !showInferred) return
      if (hiddenRels.has(relType)) return

      ctx.beginPath()
      ctx.moveTo(source.x, source.y)
      ctx.lineTo(target.x, target.y)
      ctx.strokeStyle = isInferred ? edgeColor + '60' : edgeColor + 'aa'
      ctx.lineWidth = isInferred ? 1 : 1.5
      if (isInferred) {
        ctx.setLineDash([4, 4])
      } else {
        ctx.setLineDash([])
      }
      ctx.stroke()
      ctx.setLineDash([])

      // Edge label
      if (showEdgeLabels && k > 0.6 && link.label) {
        const midX = (source.x + target.x) / 2
        const midY = (source.y + target.y) / 2
        ctx.fillStyle = isInferred ? '#4b5563' : '#9ca3af'
        ctx.font = `${isInferred ? 'italic ' : ''}${Math.max(8, 10 / k)}px Inter, sans-serif`
        ctx.textAlign = 'center'
        ctx.textBaseline = 'middle'
        // Background for readability
        const text = link.label.length > 20 ? link.label.slice(0, 20) + '…' : link.label
        const tw = ctx.measureText(text).width
        ctx.fillStyle = '#030712cc'
        ctx.fillRect(midX - tw / 2 - 2, midY - 8, tw + 4, 14)
        ctx.fillStyle = isInferred ? '#4b5563' : '#9ca3af'
        ctx.fillText(text, midX, midY - 1)
      }
    })

    // Draw nodes
    nodes.forEach((node) => {
      if (!node.x || !node.y) return
      if (hiddenTypes.has(node.entityType)) return
      const isSelected = selectedNode?.id === node.id

      const isOnPath = highlightedPath.has(node.id)

      if (isSelected || isOnPath) {
        ctx.beginPath()
        ctx.arc(node.x, node.y, node.radius + 6, 0, 2 * Math.PI)
        ctx.fillStyle = isOnPath ? 'rgba(234,179,8,0.2)' : 'rgba(59,130,246,0.15)'
        ctx.fill()
      }

      ctx.beginPath()
      ctx.arc(node.x, node.y, node.radius, 0, 2 * Math.PI)
      ctx.fillStyle = isOnPath ? '#eab308' : node.color
      ctx.fill()
      ctx.strokeStyle = isOnPath ? '#fbbf24' : isSelected ? '#60a5fa' : 'rgba(255,255,255,0.2)'
      ctx.lineWidth = isOnPath || isSelected ? 2.5 : 1
      ctx.stroke()

      if (k > 0.6 || nodes.length < 30) {
        ctx.fillStyle = '#e5e7eb'
        ctx.font = `bold ${Math.max(8, 11 / k)}px Inter, sans-serif`
        ctx.textAlign = 'center'
        ctx.textBaseline = 'top'
        const label = node.displayLabel.length > 15 ? node.displayLabel.slice(0, 15) + '…' : node.displayLabel
        ctx.fillText(label, node.x, node.y + node.radius + 3)
        // Show count for schema nodes
        if (node.properties.__type__ === 'schema' && node.properties.count) {
          ctx.fillStyle = '#9ca3af'
          ctx.font = `${Math.max(7, 9 / k)}px Inter, sans-serif`
          ctx.fillText((node.properties.count as number).toLocaleString(), node.x, node.y + node.radius + 16)
        }
      }
    })

    ctx.restore()
  }, [selectedNode])

  // --- Mouse: drag nodes / pan ---
  const handleMouseDown = (e: React.MouseEvent) => {
    const rect = canvasRef.current?.getBoundingClientRect()
    if (!rect) return
    const { x: tx, y: ty, k } = transformRef.current
    const mx = (e.clientX - rect.left - tx) / k
    const my = (e.clientY - rect.top - ty) / k

    const clicked = nodesRef.current.find((n) => n.x !== undefined && n.y !== undefined && Math.hypot(n.x - mx, n.y - my) < n.radius + 4)
    if (clicked) {
      dragRef.current = { node: clicked, offsetX: mx - clicked.x!, offsetY: my - clicked.y! }
      handleNodeClickWithPath(clicked)
      clicked.fx = clicked.x
      clicked.fy = clicked.y
    } else {
      panRef.current = { active: true, startX: e.clientX - tx, startY: e.clientY - ty }
      setSelectedNode(null)
    }
  }

  const handleMouseMove = (e: React.MouseEvent) => {
    const rect = canvasRef.current?.getBoundingClientRect()
    if (!rect) return
    const { x: tx, y: ty, k } = transformRef.current

    if (dragRef.current.node) {
      const mx = (e.clientX - rect.left - tx) / k
      const my = (e.clientY - rect.top - ty) / k
      dragRef.current.node.fx = mx - dragRef.current.offsetX
      dragRef.current.node.fy = my - dragRef.current.offsetY
      simRef.current?.alpha(0.1).restart()
    } else if (panRef.current.active) {
      transformRef.current.x = e.clientX - panRef.current.startX
      transformRef.current.y = e.clientY - panRef.current.startY
      scheduleDraw()
    }
  }

  const handleMouseUp = () => {
    if (dragRef.current.node) {
      const node = dragRef.current.node
      setTimeout(() => { node.fx = null; node.fy = null }, 2000)
    }
    dragRef.current = { node: null, offsetX: 0, offsetY: 0 }
    panRef.current.active = false
  }

  const handleDoubleClick = (e: React.MouseEvent) => {
    const rect = canvasRef.current?.getBoundingClientRect()
    if (!rect) return
    const { x: tx, y: ty, k } = transformRef.current
    const mx = (e.clientX - rect.left - tx) / k
    const my = (e.clientY - rect.top - ty) / k
    const clicked = nodesRef.current.find((n) => n.x !== undefined && n.y !== undefined && Math.hypot(n.x - mx, n.y - my) < n.radius + 4)
    if (clicked) {
      expandNode(clicked)
    }
  }

  const handleWheel = (e: React.WheelEvent) => {
    e.preventDefault()
    const rect = canvasRef.current?.getBoundingClientRect()
    if (!rect) return
    const mx = e.clientX - rect.left
    const my = e.clientY - rect.top
    const { x: tx, y: ty, k } = transformRef.current
    const factor = e.deltaY > 0 ? 0.9 : 1.1
    const newK = Math.max(0.1, Math.min(5, k * factor))
    transformRef.current = {
      x: mx - (mx - tx) * (newK / k),
      y: my - (my - ty) * (newK / k),
      k: newK,
    }
    scheduleDraw()
  }

  // --- Export PNG ---
  const exportPNG = useCallback(() => {
    const canvas = canvasRef.current
    if (!canvas) return
    const link = document.createElement('a')
    link.download = `ontodb-graph-${Date.now()}.png`
    link.href = canvas.toDataURL('image/png')
    link.click()
  }, [])

  // --- Toggle entity type filter ---
  const toggleType = useCallback((type: string) => {
    setHiddenTypes(prev => {
      const next = new Set(prev)
      if (next.has(type)) next.delete(type)
      else next.add(type)
      return next
    })
    scheduleDraw()
  }, [scheduleDraw])

  // --- Toggle relationship type filter ---
  const toggleRel = useCallback((rel: string) => {
    setHiddenRels(prev => {
      const next = new Set(prev)
      if (next.has(rel)) next.delete(rel)
      else next.add(rel)
      return next
    })
    scheduleDraw()
  }, [scheduleDraw])

  // --- BFS shortest path ---
  const findPath = useCallback((startId: string, endId: string) => {
    const links = linksRef.current
    const adj = new Map<string, string[]>()
    links.forEach(l => {
      const sid = typeof l.source === 'string' ? l.source : (l.source as SimNode).id
      const tid = typeof l.target === 'string' ? l.target : (l.target as SimNode).id
      if (!adj.has(sid)) adj.set(sid, [])
      if (!adj.has(tid)) adj.set(tid, [])
      adj.get(sid)!.push(tid)
      adj.get(tid)!.push(sid)
    })

    // BFS
    const queue = [startId]
    const visited = new Set<string>([startId])
    const parent = new Map<string, string>()
    let found = false

    while (queue.length > 0) {
      const curr = queue.shift()!
      if (curr === endId) { found = true; break }
      for (const neighbor of (adj.get(curr) || [])) {
        if (!visited.has(neighbor)) {
          visited.add(neighbor)
          parent.set(neighbor, curr)
          queue.push(neighbor)
        }
      }
    }

    if (!found) {
      setHighlightedPath(new Set())
      setError('未找到路径')
      return
    }

    // Reconstruct path
    const pathNodes = new Set<string>()
    let cur = endId
    while (cur !== startId) {
      pathNodes.add(cur)
      cur = parent.get(cur)!
    }
    pathNodes.add(startId)
    setHighlightedPath(pathNodes)
    setError(null)
  }, [])

  // --- Expand neighbors for a node ---
  const expandNode = useCallback(async (node: SimNode) => {
    if (node.properties.__type__ === 'schema') return // Don't expand schema nodes
    setLoading(true)
    try {
      const entityId = node.id
      const escaped = entityId.replace(/'/g, "''")
      const idList = `'${escaped}'`

      // Query all relationship tables in parallel
      const relQueries = RELATIONSHIP_TABLES.map(table => ({
        table,
        sql: `SELECT source, target FROM sembio.${table} WHERE source = ${idList} OR target = ${idList} LIMIT 10`,
      }))

      const results = await Promise.allSettled(
        relQueries.map(r => sqlQuery<Record<string, string>>(r.sql))
      )

      const newEdges: Array<{ from: string; to: string; label: string }> = []
      const newEntityIds = new Set<string>()

      results.forEach((result, i) => {
        if (result.status === 'fulfilled') {
          result.value.forEach(row => {
            if (row.source && row.target) {
              newEdges.push({ from: row.source, to: row.target, label: relQueries[i].table })
              if (row.source !== entityId) newEntityIds.add(row.source)
              if (row.target !== entityId) newEntityIds.add(row.target)
            }
          })
        }
      })

      // Filter out entities we already have
      const existingIds = new Set(nodesRef.current.map(n => n.id))
      const toFetch = Array.from(newEntityIds).filter(id => !existingIds.has(id))

      if (toFetch.length === 0 && newEdges.length === 0) {
        setError('没有找到更多邻居')
        setLoading(false)
        return
      }

      // Fetch new entity details
      const typeGroups = new Map<string, string[]>()
      toFetch.forEach(id => {
        const type = getEntityType(id)
        if (!typeGroups.has(type)) typeGroups.set(type, [])
        typeGroups.get(type)!.push(id)
      })

      const newNodes: Array<{ id: string; properties: Record<string, unknown> }> = []
      for (const [type, ids] of typeGroups) {
        if (ids.length === 0) continue
        const idListStr = ids.map(id => `'${id.replace(/'/g, "''")}'`).join(',')
        try {
          const rows = await sqlQuery<Record<string, unknown>>(`SELECT * FROM sembio.${type} WHERE id IN (${idListStr}) LIMIT ${ids.length}`)
          rows.forEach(row => {
            if (row.id) newNodes.push({ id: row.id as string, properties: row })
          })
        } catch {
          ids.forEach(id => newNodes.push({ id, properties: {} }))
        }
      }

      // Add to existing graph
      const allNodes = [...nodesRef.current.map(n => ({ id: n.id, properties: n.properties })), ...newNodes]
      const allEdges = [...linksRef.current.map(l => ({
        from: typeof l.source === 'string' ? l.source : (l.source as SimNode).id,
        to: typeof l.target === 'string' ? l.target : (l.target as SimNode).id,
        label: l.label || '',
      })), ...newEdges]

      buildGraph(allNodes, allEdges)
    } catch (err) {
      setError(`展开失败: ${err}`)
    } finally {
      setLoading(false)
    }
  }, [buildGraph])

  useEffect(() => { scheduleDraw() }, [selectedNode, scheduleDraw])
  useEffect(() => { scheduleDraw() }, [scheduleDraw])

  // --- Auto-load schema overview on mount (real counts from API) ---
  useEffect(() => {
    const loadOverview = async () => {
      const entityTypes = [
        'Drug', 'Disease', 'Gene', 'Protein', 'MiRNA', 'Target', 'Enzyme',
        'Kinase', 'Cancer', 'Metabolite', 'Lipid', 'CellLine', 'Pathway',
        'Tissue', 'ProteinClass', 'ClinicalTrial',
      ]
      const relDefs = [
        { table: 'treats', from: 'Drug', to: 'Disease' },
        { table: 'targets', from: 'MiRNA', to: 'Gene' },
        { table: 'interacts_with', from: 'Drug', to: 'Drug' },
        { table: 'causes', from: 'Disease', to: 'Disease' },
        { table: 'catalyzes', from: 'Enzyme', to: 'Drug' },
        { table: 'regulates', from: 'Protein', to: 'Gene' },
        { table: 'associated_with', from: 'Gene', to: 'Disease' },
        { table: 'metabolizes', from: 'Enzyme', to: 'Drug' },
        { table: 'participates_in', from: 'Protein', to: 'Pathway' },
        { table: 'has_bioactivity', from: 'Drug', to: 'Protein' },
        { table: 'mirna_disease', from: 'MiRNA', to: 'Disease' },
        { table: 'disease_target', from: 'Disease', to: 'Target' },
      ]

      // Fetch entity counts in parallel (all COUNT(*) hit cache, ~1ms each)
      const countQueries = entityTypes.map(type =>
        sqlQuery<Record<string, number>>(`SELECT COUNT(*) as cnt FROM sembio.${type}`)
          .then(rows => ({ type, count: rows[0]?.cnt ?? 0 }))
          .catch(() => ({ type, count: 0 }))
      )
      const relQueries = relDefs.map(rel =>
        sqlQuery<Record<string, number>>(`SELECT COUNT(*) as cnt FROM sembio.${rel.table}`)
          .then(rows => ({ ...rel, count: rows[0]?.cnt ?? 0 }))
          .catch(() => ({ ...rel, count: 0 }))
      )

      const [entityCounts, relCounts] = await Promise.all([
        Promise.all(countQueries),
        Promise.all(relQueries),
      ])

      const nodes = entityCounts
        .filter(e => e.count > 0)
        .map(e => ({ id: `type::${e.type}`, label: e.type, count: e.count }))

      const edges = relCounts
        .filter(r => r.count > 0)
        .map(r => ({ from: `type::${r.from}`, to: `type::${r.to}`, label: r.table }))

      if (nodes.length > 0) {
        buildGraphFromSkeleton(nodes, edges)
      }
    }
    loadOverview()
  }, [])

  const buildGraphFromSkeleton = useCallback((nodes: Array<{ id: string; label: string; count: number }>, edges: Array<{ from: string; to: string; label: string }>) => {
    const canvas = canvasRef.current
    if (!canvas) return
    const w = canvas.offsetWidth
    const h = canvas.offsetHeight
    const maxCount = Math.max(...nodes.map(n => n.count), 1)

    const nodeMap = new Map<string, SimNode>()
    const simNodes: SimNode[] = nodes.map(n => {
      const entityType = n.label
      const node: SimNode = {
        id: n.id,
        label: entityType,
        properties: { __type__: 'schema', entity_type: entityType, count: n.count },
        entityType,
        color: ENTITY_COLORS[entityType] || DEFAULT_COLOR,
        displayLabel: entityType,
        radius: Math.max(14, Math.min(44, 14 + Math.sqrt(n.count / maxCount) * 30)),
        x: w / 2 + (Math.random() - 0.5) * 400,
        y: h / 2 + (Math.random() - 0.5) * 400,
      }
      nodeMap.set(n.id, node)
      return node
    })

    const simLinks: SimLink[] = edges
      .filter(e => nodeMap.has(e.from) && nodeMap.has(e.to) && e.from !== e.to)
      .map(e => ({ source: e.from, target: e.to, label: e.label }))

    nodesRef.current = simNodes
    linksRef.current = simLinks
    setNodeCount(simNodes.length)
    setEdgeCount(simLinks.length)

    if (simRef.current) simRef.current.stop()
    const sim = forceSimulation(simNodes)
      .force('link', forceLink<SimNode, SimLink>(simLinks).id(d => d.id).distance(160).strength(0.3))
      .force('charge', forceManyBody().strength(-600))
      .force('center', forceCenter(w / 2, h / 2).strength(0.05))
      .force('collide', forceCollide<SimNode>().radius(d => d.radius + 12).strength(0.7))
      .alpha(1)
      .alphaDecay(0.02)
    sim.on('tick', () => scheduleDraw())
    simRef.current = sim
    transformRef.current = { x: 0, y: 0, k: 1 }
  }, [])

  // --- Load schema overview (real counts from API) ---
  const loadSchemaOverview = useCallback(async () => {
    setLoading(true)
    try {
      const entityTypes = [
        'Drug', 'Disease', 'Gene', 'Protein', 'MiRNA', 'Target', 'Enzyme',
        'Kinase', 'Cancer', 'Metabolite', 'Lipid', 'CellLine', 'Pathway',
        'Tissue', 'ProteinClass', 'ClinicalTrial',
      ]
      const relDefs = [
        { table: 'treats', from: 'Drug', to: 'Disease' },
        { table: 'targets', from: 'MiRNA', to: 'Gene' },
        { table: 'interacts_with', from: 'Drug', to: 'Drug' },
        { table: 'causes', from: 'Disease', to: 'Disease' },
        { table: 'catalyzes', from: 'Enzyme', to: 'Drug' },
        { table: 'regulates', from: 'Protein', to: 'Gene' },
        { table: 'associated_with', from: 'Gene', to: 'Disease' },
        { table: 'metabolizes', from: 'Enzyme', to: 'Drug' },
        { table: 'participates_in', from: 'Protein', to: 'Pathway' },
        { table: 'has_bioactivity', from: 'Drug', to: 'Protein' },
        { table: 'mirna_disease', from: 'MiRNA', to: 'Disease' },
        { table: 'disease_target', from: 'Disease', to: 'Target' },
      ]
      const countQueries = entityTypes.map(type =>
        sqlQuery<Record<string, number>>(`SELECT COUNT(*) as cnt FROM sembio.${type}`)
          .then(rows => ({ type, count: rows[0]?.cnt ?? 0 }))
          .catch(() => ({ type, count: 0 }))
      )
      const relQueries = relDefs.map(rel =>
        sqlQuery<Record<string, number>>(`SELECT COUNT(*) as cnt FROM sembio.${rel.table}`)
          .then(rows => ({ ...rel, count: rows[0]?.cnt ?? 0 }))
          .catch(() => ({ ...rel, count: 0 }))
      )
      const [entityCounts, relCounts] = await Promise.all([
        Promise.all(countQueries),
        Promise.all(relQueries),
      ])
      const nodes = entityCounts.filter(e => e.count > 0).map(e => ({ id: `type::${e.type}`, label: e.type, count: e.count }))
      const edges = relCounts.filter(r => r.count > 0).map(r => ({ from: `type::${r.from}`, to: `type::${r.to}`, label: r.table }))
      if (nodes.length > 0) buildGraphFromSkeleton(nodes, edges)
    } catch (err) {
      setError(`加载失败: ${err}`)
    } finally {
      setLoading(false)
    }
  }, [])

  // --- Drill down: click a type node to load entities of that type ---
  const handleNodeClick = useCallback(async (node: SimNode) => {
    setSelectedNode(node)
    if (node.properties.__type__ === 'schema') {
      const entityType = node.properties.entity_type as string
      setLoading(true)
      try {
        const nameCol = entityType === 'Drug' ? 'drug_name' : entityType === 'Disease' ? 'disease_name' : entityType === 'Gene' ? 'gene_name' : 'name'
        // Load entities
        const entities = await sqlQuery<Record<string, unknown>>(
          `SELECT id, ${nameCol} FROM sembio.${entityType} WHERE ${nameCol} IS NOT NULL AND ${nameCol} != '' LIMIT 20`
        )
        const entityIds = entities.map(e => e.id as string).filter(Boolean)
        const allEdges: Array<{ from: string; to: string; label: string }> = []

        // Batch query relationships: find edges where any of these entities appear
        if (entityIds.length > 0) {
          const idList = entityIds.map(id => `'${id.replace(/'/g, "''")}'`).join(',')
          // Query treats and targets in parallel
          const [treatsR, targetsR] = await Promise.all([
            sqlQuery<Record<string, string>>(`SELECT source, target FROM sembio.treats WHERE source IN (${idList}) OR target IN (${idList}) LIMIT 30`),
            sqlQuery<Record<string, string>>(`SELECT source, target FROM sembio.targets WHERE source IN (${idList}) OR target IN (${idList}) LIMIT 30`),
          ])
          treatsR.forEach(r => { if (r.source && r.target) allEdges.push({ from: r.source, to: r.target, label: 'treats' }) })
          targetsR.forEach(r => { if (r.source && r.target) allEdges.push({ from: r.source, to: r.target, label: 'targets' }) })
        }

        // Build nodes: fetched entities + any referenced entities from edges
        const allIds = new Set(entityIds)
        allEdges.forEach(e => { allIds.add(e.from); allIds.add(e.to) })
        const nodes = entities.map(e => ({ id: e.id as string, properties: e }))
        allIds.forEach(id => {
          if (!nodes.find(n => n.id === id)) {
            nodes.push({ id, properties: {} })
          }
        })

        buildGraph(nodes, allEdges)
      } catch (err) {
        setError(`钻取失败: ${err}`)
      } finally {
        setLoading(false)
      }
    }
  }, [buildGraph])

  // --- Handle node click with path mode ---
  const handleNodeClickWithPath = useCallback((node: SimNode) => {
    if (pathMode) {
      if (!pathStart) {
        setPathStart(node.id)
        setSelectedNode(node)
      } else if (!pathEnd) {
        setPathEnd(node.id)
        findPath(pathStart, node.id)
      } else {
        setPathStart(node.id)
        setPathEnd(null)
        setHighlightedPath(new Set())
        setSelectedNode(node)
      }
    } else {
      handleNodeClick(node)
    }
  }, [pathMode, pathStart, pathEnd, findPath, handleNodeClick])

  const legendTypes = Object.entries(ENTITY_COLORS).slice(0, 15)

  return (
    <div className="flex h-full">
      {/* Sidebar */}
      <div className="w-72 border-r border-gray-800 bg-gray-900 p-4 space-y-4 overflow-y-auto">
        <h3 className="text-white font-medium text-sm">知识图谱浏览器</h3>

        {/* Search */}
        <div>
          <label className="block text-xs text-gray-400 mb-1">语义搜索</label>
          <div className="flex gap-1">
            <input
              value={searchQuery}
              onChange={(e) => setSearchQuery(e.target.value)}
              onKeyDown={(e) => e.key === 'Enter' && handleSearch()}
              className="flex-1 bg-gray-800 text-white text-sm rounded px-3 py-1.5 border border-gray-700 outline-none focus:border-blue-500"
              placeholder="输入实体名称..."
            />
            <button
              onClick={handleSearch}
              disabled={searching}
              className="px-3 py-1.5 bg-gray-700 hover:bg-gray-600 text-white text-sm rounded"
            >
              {searching ? '...' : '🔍'}
            </button>
          </div>
          {searchResults.length > 0 && (
            <div className="mt-2 max-h-48 overflow-y-auto space-y-1">
              {searchResults.map((r, i) => (
                <button
                  key={i}
                  onClick={() => {
                    setSearchResults([])
                    handleTraverse(r.id)
                  }}
                  className="w-full text-left p-2 bg-gray-800 hover:bg-gray-700 rounded text-xs transition-colors"
                >
                  <div className="flex items-center gap-2">
                    <span className="w-2 h-2 rounded-full shrink-0" style={{ backgroundColor: ENTITY_COLORS[r.type] || DEFAULT_COLOR }} />
                    <span className="text-gray-200 truncate">{r.name}</span>
                  </div>
                  <span className="text-gray-500 ml-4">{r.type}</span>
                </button>
              ))}
            </div>
          )}
        </div>

        {/* Controls */}
        <div className="grid grid-cols-2 gap-2">
          <div>
            <label className="block text-xs text-gray-400 mb-1">遍历深度</label>
            <input
              type="number"
              value={depth}
              onChange={(e) => setDepth(Math.max(1, Math.min(3, parseInt(e.target.value) || 1)))}
              min={1}
              max={3}
              className="w-full bg-gray-800 text-white text-sm rounded px-3 py-1.5 border border-gray-700 outline-none focus:border-blue-500"
            />
          </div>
          <div>
            <label className="block text-xs text-gray-400 mb-1">每节点邻居</label>
            <input
              type="number"
              value={maxNeighbors}
              onChange={(e) => setMaxNeighbors(Math.max(1, Math.min(50, parseInt(e.target.value) || 10)))}
              min={1}
              max={50}
              className="w-full bg-gray-800 text-white text-sm rounded px-3 py-1.5 border border-gray-700 outline-none focus:border-blue-500"
            />
          </div>
        </div>

        {/* Display options */}
        <div className="space-y-2">
          <label className="flex items-center gap-2 text-xs text-gray-400 cursor-pointer">
            <input type="checkbox" checked={showEdgeLabels} onChange={(e) => setShowEdgeLabels(e.target.checked)} className="rounded" />
            显示关系标签
          </label>
          <label className="flex items-center gap-2 text-xs text-gray-400 cursor-pointer">
            <input type="checkbox" checked={showInferred} onChange={(e) => setShowInferred(e.target.checked)} className="rounded" />
            显示推导关系（虚线）
          </label>
        </div>

        {/* Path finding */}
        <div className="p-2 bg-gray-800/50 rounded space-y-2">
          <label className="flex items-center gap-2 text-xs text-gray-400 cursor-pointer">
            <input type="checkbox" checked={pathMode} onChange={(e) => {
              setPathMode(e.target.checked)
              setPathStart(null)
              setPathEnd(null)
              setHighlightedPath(new Set())
            }} className="rounded" />
            路径查找模式
          </label>
          {pathMode && (
            <div className="text-xs text-gray-500">
              {!pathStart && '点击第一个节点作为起点'}
              {pathStart && !pathEnd && `起点: ${pathStart.split('::').pop()} → 点击终点`}
              {pathStart && pathEnd && `路径: ${pathStart.split('::').pop()} → ${pathEnd.split('::').pop()}`}
            </div>
          )}
        </div>

        {loading && (
          <div className="text-center text-blue-400 text-sm animate-pulse">遍历中...</div>
        )}

        {error && (
          <div className="p-2 bg-red-900/30 border border-red-700 rounded text-red-400 text-xs">{error}</div>
        )}

        <div className="text-xs text-gray-500">
          节点: {nodeCount} · 边: {edgeCount}
        </div>

        {/* Graph statistics */}
        {nodeCount > 0 && (
          <div className="p-2 bg-gray-800/50 rounded text-xs space-y-1">
            <div className="text-gray-400 font-medium mb-1">图谱统计</div>
            <div className="text-gray-500">节点类型: {new Set(nodesRef.current.map(n => n.entityType)).size}</div>
            <div className="text-gray-500">关系类型: {new Set(linksRef.current.map(l => (l.label || '').split(' ')[0])).size}</div>
            <div className="text-gray-500">平均度数: {nodeCount > 0 ? (edgeCount * 2 / nodeCount).toFixed(1) : 0}</div>
          </div>
        )}

        {/* Back to overview / Load overview */}
        {nodeCount === 0 && !loading && (
          <button
            onClick={loadSchemaOverview}
            className="w-full px-4 py-2 bg-emerald-600 hover:bg-emerald-700 text-white text-sm rounded font-medium transition-colors"
          >
            加载整体图谱
          </button>
        )}
        {nodeCount > 0 && (
          <button
            onClick={loadSchemaOverview}
            className="w-full px-3 py-1.5 bg-gray-700 hover:bg-gray-600 text-white text-xs rounded"
          >
            ← 返回整体视图
          </button>
        )}

        {/* Selected node */}
        {selectedNode && (
          <div className="p-3 bg-gray-800 rounded border border-gray-700">
            <div className="flex items-center gap-2 mb-2">
              <span className="w-3 h-3 rounded-full" style={{ backgroundColor: selectedNode.color }} />
              <h4 className="text-white text-sm font-medium">{selectedNode.displayLabel}</h4>
            </div>
            {selectedNode.properties.__type__ === 'schema' ? (
              <>
                <div className="text-xs text-blue-400 mb-2">实体类型</div>
                <div className="text-xs text-gray-300 mb-2">
                  实体数量: {(selectedNode.properties.count as number)?.toLocaleString() ?? 0}
                </div>
                <div className="text-xs text-gray-500">点击可钻取查看具体实体</div>
              </>
            ) : (
              <>
                <div className="text-xs text-blue-400 mb-2">{selectedNode.entityType}</div>
                <div className="text-xs text-gray-500 mb-2 break-all">ID: {selectedNode.id}</div>
                <div className="space-y-1 max-h-32 overflow-y-auto">
                  {Object.entries(selectedNode.properties).filter(([k]) => !k.startsWith('__')).slice(0, 10).map(([k, v]) => (
                    <div key={k} className="text-xs">
                      <span className="text-gray-400">{k}: </span>
                      <span className="text-gray-200 break-all">{String(v).slice(0, 80)}</span>
                    </div>
                  ))}
                </div>
                <div className="mt-2 grid grid-cols-2 gap-1">
                  <button
                    onClick={() => handleTraverse(selectedNode.id)}
                    className="px-3 py-1.5 bg-blue-600 hover:bg-blue-700 text-white text-xs rounded"
                  >
                    遍历
                  </button>
                  <button
                    onClick={() => expandNode(selectedNode)}
                    className="px-3 py-1.5 bg-emerald-600 hover:bg-emerald-700 text-white text-xs rounded"
                  >
                    展开邻居
                  </button>
                </div>
                <div className="mt-1 text-xs text-gray-600">双击节点也可展开</div>
              </>
            )}
          </div>
        )}

        {/* Legend + Filters */}
        <div>
          <label className="block text-xs text-gray-400 mb-2">实体类型（点击过滤）</label>
          <div className="grid grid-cols-2 gap-1">
            {legendTypes.map(([type, color]) => (
              <button
                key={type}
                onClick={() => toggleType(type)}
                className={`flex items-center gap-1.5 text-xs px-1 py-0.5 rounded transition-colors ${
                  hiddenTypes.has(type) ? 'text-gray-600 line-through' : 'text-gray-400 hover:bg-gray-800'
                }`}
              >
                <span className="w-2 h-2 rounded-full shrink-0" style={{ backgroundColor: hiddenTypes.has(type) ? '#374151' : color }} />
                <span className="truncate">{type}</span>
              </button>
            ))}
          </div>
        </div>
        <div>
          <label className="block text-xs text-gray-400 mb-2">关系类型（点击过滤）</label>
          <div className="grid grid-cols-2 gap-1">
            {Object.entries(REL_COLORS).slice(0, 12).map(([type, color]) => (
              <button
                key={type}
                onClick={() => toggleRel(type)}
                className={`flex items-center gap-1.5 text-xs px-1 py-0.5 rounded transition-colors ${
                  hiddenRels.has(type) ? 'text-gray-600 line-through' : 'text-gray-400 hover:bg-gray-800'
                }`}
              >
                <span className="w-4 h-0.5 shrink-0" style={{ backgroundColor: hiddenRels.has(type) ? '#374151' : color }} />
                <span className="truncate">{type}</span>
              </button>
            ))}
          </div>
        </div>

        {/* Export */}
        <button
          onClick={exportPNG}
          className="w-full px-3 py-1.5 bg-gray-700 hover:bg-gray-600 text-white text-xs rounded"
        >
          导出 PNG
        </button>
      </div>

      {/* Canvas */}
      <canvas
        ref={canvasRef}
        className="flex-1 cursor-grab active:cursor-grabbing"
        onMouseDown={handleMouseDown}
        onMouseMove={handleMouseMove}
        onMouseUp={handleMouseUp}
        onMouseLeave={handleMouseUp}
        onDoubleClick={handleDoubleClick}
        onWheel={handleWheel}
      />
    </div>
  )
}
import { useRef, useMemo } from 'react'
import { Canvas, useFrame } from '@react-three/fiber'
import { OrbitControls, Text, Line } from '@react-three/drei'
import * as THREE from 'three'
import { useDashboardStore } from '../stores/dashboard'
import type { TopologyNode, TopologyEdge } from '../types'

const NODE_COLORS = {
  healthy: '#22c55e',
  degraded: '#eab308',
  unhealthy: '#ef4444',
}

const ROLE_LABELS = {
  leader: 'LEADER',
  follower: 'FOLLOWER',
  standalone: 'STANDALONE',
}

function NodeSphere({ node }: { node: TopologyNode }) {
  const meshRef = useRef<THREE.Mesh>(null)
  const glowRef = useRef<THREE.Mesh>(null)
  const selectedNode = useDashboardStore((s) => s.selectedNode)
  const setSelectedNode = useDashboardStore((s) => s.setSelectedNode)
  const isSelected = selectedNode === node.id
  const color = NODE_COLORS[node.health]

  useFrame((_, delta) => {
    if (meshRef.current) {
      meshRef.current.rotation.y += delta * 0.3
    }
    if (glowRef.current) {
      const scale = 1 + Math.sin(Date.now() * 0.003) * 0.08
      glowRef.current.scale.setScalar(scale)
    }
  })

  return (
    <group position={node.position}>
      {/* Glow sphere */}
      <mesh ref={glowRef}>
        <sphereGeometry args={[0.35, 32, 32]} />
        <meshBasicMaterial color={color} transparent opacity={0.12} />
      </mesh>
      {/* Main sphere */}
      <mesh
        ref={meshRef}
        onClick={(e) => {
          e.stopPropagation()
          setSelectedNode(isSelected ? null : node.id)
        }}
      >
        <sphereGeometry args={[0.25, 32, 32]} />
        <meshStandardMaterial
          color={color}
          emissive={color}
          emissiveIntensity={isSelected ? 0.8 : 0.3}
          metalness={0.6}
          roughness={0.3}
        />
      </mesh>
      {/* Selection ring */}
      {isSelected && (
        <mesh rotation={[Math.PI / 2, 0, 0]}>
          <ringGeometry args={[0.35, 0.4, 32]} />
          <meshBasicMaterial color="#3b82f6" transparent opacity={0.6} side={THREE.DoubleSide} />
        </mesh>
      )}
      {/* Role label */}
      <Text
        position={[0, 0.5, 0]}
        fontSize={0.12}
        color={color}
        anchorX="center"
        anchorY="bottom"
        font="/fonts/inter.woff2"
      >
        {ROLE_LABELS[node.role]}
      </Text>
      {/* Node ID label */}
      <Text
        position={[0, -0.4, 0]}
        fontSize={0.1}
        color="#94a3b8"
        anchorX="center"
        anchorY="top"
        font="/fonts/inter.woff2"
      >
        {node.label}
      </Text>
    </group>
  )
}

function ConnectionLine({ edge, nodes }: { edge: TopologyEdge; nodes: TopologyNode[] }) {
  const fromNode = nodes.find((n) => n.id === edge.from)
  const toNode = nodes.find((n) => n.id === edge.to)
  if (!fromNode || !toNode) return null

  const points = useMemo(
    () => [
      new THREE.Vector3(...fromNode.position),
      new THREE.Vector3(...toNode.position),
    ],
    [fromNode.position, toNode.position],
  )

  return (
    <Line
      points={points}
      color="#3b82f6"
      lineWidth={1.5}
      transparent
      opacity={0.4}
      dashed={edge.type === 'replication'}
      dashSize={0.1}
      gapSize={0.05}
    />
  )
}

function DataFlowParticles({ edge, nodes }: { edge: TopologyEdge; nodes: TopologyNode[] }) {
  const fromNode = nodes.find((n) => n.id === edge.from)
  const toNode = nodes.find((n) => n.id === edge.to)
  const ref = useRef<THREE.Points>(null)

  const { positions, velocities } = useMemo(() => {
    if (!fromNode || !toNode) return { positions: new Float32Array(0), velocities: [] as number[] }
    const count = 8
    const pos = new Float32Array(count * 3)
    const vel: number[] = []
    for (let i = 0; i < count; i++) {
      const t = i / count
      pos[i * 3] = fromNode.position[0] + (toNode.position[0] - fromNode.position[0]) * t
      pos[i * 3 + 1] = fromNode.position[1] + (toNode.position[1] - fromNode.position[1]) * t
      pos[i * 3 + 2] = fromNode.position[2] + (toNode.position[2] - fromNode.position[2]) * t
      vel.push(t)
    }
    return { positions: pos, velocities: vel }
  }, [fromNode, toNode])

  useFrame((_, delta) => {
    if (!ref.current || !fromNode || !toNode) return
    const posAttr = ref.current.geometry.attributes.position
    const arr = posAttr.array as Float32Array
    for (let i = 0; i < velocities.length; i++) {
      velocities[i] = (velocities[i] + delta * 0.3) % 1
      const t = velocities[i]
      arr[i * 3] = fromNode.position[0] + (toNode.position[0] - fromNode.position[0]) * t
      arr[i * 3 + 1] = fromNode.position[1] + (toNode.position[1] - fromNode.position[1]) * t
      arr[i * 3 + 2] = fromNode.position[2] + (toNode.position[2] - fromNode.position[2]) * t
    }
    posAttr.needsUpdate = true
  })

  if (!fromNode || !toNode) return null

  return (
    <points ref={ref}>
      <bufferGeometry>
        <bufferAttribute
          attach="attributes-position"
          count={positions.length / 3}
          array={positions}
          itemSize={3}
        />
      </bufferGeometry>
      <pointsMaterial size={0.06} color="#3b82f6" transparent opacity={0.8} sizeAttenuation />
    </points>
  )
}

function GridFloor() {
  return (
    <gridHelper
      args={[20, 20, '#1e293b', '#111827']}
      position={[0, -1.5, 0]}
    />
  )
}

export function Topology3D() {
  const nodes = useDashboardStore((s) => s.topologyNodes)
  const edges = useDashboardStore((s) => s.topologyEdges)

  return (
    <Canvas
      camera={{ position: [5, 4, 5], fov: 50 }}
      style={{ background: 'transparent' }}
      onPointerMissed={() => useDashboardStore.getState().setSelectedNode(null)}
    >
      <ambientLight intensity={0.3} />
      <pointLight position={[10, 10, 10]} intensity={0.8} color="#3b82f6" />
      <pointLight position={[-10, -5, -10]} intensity={0.4} color="#a855f7" />

      <GridFloor />

      {nodes.map((node) => (
        <NodeSphere key={node.id} node={node} />
      ))}

      {edges.map((edge, i) => (
        <ConnectionLine key={i} edge={edge} nodes={nodes} />
      ))}

      {edges.map((edge, i) => (
        <DataFlowParticles key={`p-${i}`} edge={edge} nodes={nodes} />
      ))}

      <OrbitControls
        enablePan
        enableZoom
        enableRotate
        minDistance={2}
        maxDistance={15}
        autoRotate
        autoRotateSpeed={0.3}
      />
    </Canvas>
  )
}

interface MiniChartProps {
  data: number[]
  width?: number
  height?: number
  color?: string
  showDots?: boolean
}

export function MiniChart({ data, width = 200, height = 40, color = '#3b82f6', showDots = false }: MiniChartProps) {
  if (data.length < 2) return <div className="text-gray-500 text-xs">数据不足</div>

  const min = Math.min(...data)
  const max = Math.max(...data)
  const range = max - min || 1
  const padding = 2

  const points = data.map((value, index) => ({
    x: padding + (index / (data.length - 1)) * (width - 2 * padding),
    y: height - padding - ((value - min) / range) * (height - 2 * padding),
  }))

  const pathD = points.map((p, i) => `${i === 0 ? 'M' : 'L'} ${p.x} ${p.y}`).join(' ')
  const areaD = `${pathD} L ${points[points.length - 1].x} ${height} L ${points[0].x} ${height} Z`

  return (
    <svg width={width} height={height} className="overflow-visible">
      {/* Area fill */}
      <path d={areaD} fill={`${color}20`} />
      {/* Line */}
      <path d={pathD} fill="none" stroke={color} strokeWidth={1.5} strokeLinejoin="round" />
      {/* Dots */}
      {showDots && points.map((p, i) => (
        <circle key={i} cx={p.x} cy={p.y} r={2} fill={color} />
      ))}
      {/* Current value dot */}
      <circle cx={points[points.length - 1].x} cy={points[points.length - 1].y} r={3} fill={color} />
    </svg>
  )
}

interface GaugeProps {
  value: number
  max: number
  label: string
  unit?: string
  color?: string
  size?: number
}

export function Gauge({ value, max, label, unit = '', color = '#3b82f6', size = 80 }: GaugeProps) {
  const percentage = Math.min((value / max) * 100, 100)
  const radius = size / 2 - 8
  const circumference = 2 * Math.PI * radius
  const strokeDashoffset = circumference - (percentage / 100) * circumference

  return (
    <div className="flex flex-col items-center gap-1">
      <svg width={size} height={size} className="-rotate-90">
        {/* Background */}
        <circle
          cx={size / 2}
          cy={size / 2}
          r={radius}
          fill="none"
          stroke="#374151"
          strokeWidth={6}
        />
        {/* Value */}
        <circle
          cx={size / 2}
          cy={size / 2}
          r={radius}
          fill="none"
          stroke={color}
          strokeWidth={6}
          strokeLinecap="round"
          strokeDasharray={circumference}
          strokeDashoffset={strokeDashoffset}
          className="transition-all duration-500"
        />
      </svg>
      <div className="text-center -mt-12">
        <div className="text-lg font-bold text-white">{value}{unit}</div>
        <div className="text-xs text-gray-400">{label}</div>
      </div>
    </div>
  )
}

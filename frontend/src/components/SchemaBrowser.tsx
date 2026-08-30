import { useState, useEffect } from 'react'

interface PropertyInfo {
  domain: string
  range: string
  required?: boolean
  multi_valued?: boolean
  is_functional?: boolean
}

interface ClassInfo {
  name: string
  type?: string
  superclasses?: string[]
  properties: string[]
}

interface OntologyInfo {
  name: string
  classes: Record<string, ClassInfo>
  properties: Record<string, PropertyInfo>
}

export function SchemaBrowser() {
  const [ontologies, setOntologies] = useState<OntologyInfo[]>([])
  const [selectedOnto, setSelectedOnto] = useState<OntologyInfo | null>(null)
  const [selectedClass, setSelectedClass] = useState<string | null>(null)
  const [loading, setLoading] = useState(true)

  useEffect(() => {
    async function loadSchema() {
      try {
        const resp = await fetch('/api/schema')
        const data = await resp.json()
        const ontoList: OntologyInfo[] = (data.data?.ontologies || []).map((o: Record<string, unknown>) => ({
          name: o.name as string,
          classes: (o.classes || {}) as Record<string, ClassInfo>,
          properties: (o.properties || {}) as Record<string, PropertyInfo>,
        }))
        setOntologies(ontoList)
      } catch (err) {
        console.error('Failed to load schema:', err)
      } finally {
        setLoading(false)
      }
    }
    loadSchema()
  }, [])

  // Get properties for a specific class
  function getClassProperties(onto: OntologyInfo, className: string): Record<string, PropertyInfo> {
    const result: Record<string, PropertyInfo> = {}
    for (const [propName, prop] of Object.entries(onto.properties)) {
      if (prop.domain === className) {
        result[propName] = prop
      }
    }
    return result
  }

  // Parse class name (remove EXTENDS part for display)
  function parseClassName(raw: string): { name: string; extends?: string } {
    const parts = raw.split(' EXTENDS ')
    return { name: parts[0].trim(), extends: parts[1]?.trim() }
  }

  if (loading) {
    return (
      <div className="flex items-center justify-center h-full text-gray-500">
        加载中...
      </div>
    )
  }

  const activeClass = selectedOnto && selectedClass
    ? parseClassName(selectedClass)
    : null
  const activeProps = selectedOnto && selectedClass
    ? getClassProperties(selectedOnto, selectedClass)
    : {}

  return (
    <div className="flex h-full">
      {/* Left: Ontology tree */}
      <div className="w-72 border-r border-gray-800 bg-gray-900 overflow-y-auto">
        <div className="p-3 border-b border-gray-800">
          <h3 className="text-white font-medium text-sm">本体浏览器</h3>
          <p className="text-gray-500 text-xs mt-1">{ontologies.length} 个本体</p>
        </div>
        <div className="py-1">
          {ontologies.map((onto) => {
            const classCount = Object.keys(onto.classes).length
            return (
              <div key={onto.name}>
                {/* Ontology header */}
                <button
                  onClick={() => {
                    setSelectedOnto(selectedOnto?.name === onto.name ? null : onto)
                    setSelectedClass(null)
                  }}
                  className={`w-full text-left px-3 py-2 text-sm transition-colors flex items-center gap-2 ${
                    selectedOnto?.name === onto.name
                      ? 'bg-blue-600/20 text-blue-400'
                      : 'text-gray-300 hover:bg-gray-800'
                  }`}
                >
                  <span className="text-yellow-500">📦</span>
                  <span className="font-medium">{onto.name}</span>
                  <span className="ml-auto text-xs text-gray-500">{classCount} 类</span>
                </button>

                {/* Classes under this ontology */}
                {selectedOnto?.name === onto.name && (
                  <div className="ml-4 border-l border-gray-700">
                    {Object.keys(onto.classes).map((rawClassName) => {
                      const { name: className, extends: parent } = parseClassName(rawClassName)
                      return (
                        <button
                          key={rawClassName}
                          onClick={() => setSelectedClass(rawClassName)}
                          className={`w-full text-left px-3 py-1.5 text-xs transition-colors flex items-center gap-2 ${
                            selectedClass === rawClassName
                              ? 'bg-blue-600/20 text-blue-400'
                              : 'text-gray-400 hover:bg-gray-800 hover:text-gray-300'
                          }`}
                        >
                          <span className="text-green-500">🔷</span>
                          <span className="font-mono">{className}</span>
                          {parent && (
                            <span className="text-gray-600 text-[10px]">extends {parent}</span>
                          )}
                        </button>
                      )
                    })}
                  </div>
                )}
              </div>
            )
          })}
        </div>
      </div>

      {/* Right: Class details */}
      <div className="flex-1 overflow-y-auto">
        {selectedOnto && activeClass ? (
          <div className="p-4">
            {/* Class header */}
            <div className="flex items-center gap-3 mb-4">
              <h2 className="text-white text-lg font-semibold">{activeClass.name}</h2>
              <span className="px-2 py-0.5 bg-yellow-900/30 text-yellow-400 text-xs rounded">
                {selectedOnto.name}
              </span>
              {activeClass.extends && (
                <span className="text-gray-500 text-sm">
                  extends <span className="text-gray-400">{activeClass.extends}</span>
                </span>
              )}
              <span className="ml-auto text-xs text-gray-500">
                {Object.keys(activeProps).length} 属性
              </span>
            </div>

            {/* Properties table */}
            {Object.keys(activeProps).length > 0 ? (
              <table className="w-full text-sm">
                <thead>
                  <tr className="bg-gray-800">
                    <th className="px-4 py-2.5 text-left text-gray-400 font-medium text-xs uppercase">#</th>
                    <th className="px-4 py-2.5 text-left text-gray-400 font-medium text-xs uppercase">属性名</th>
                    <th className="px-4 py-2.5 text-left text-gray-400 font-medium text-xs uppercase">类型</th>
                    <th className="px-4 py-2.5 text-left text-gray-400 font-medium text-xs uppercase">必填</th>
                    <th className="px-4 py-2.5 text-left text-gray-400 font-medium text-xs uppercase">多值</th>
                  </tr>
                </thead>
                <tbody>
                  {Object.entries(activeProps).map(([name, prop], i) => (
                    <tr key={name} className="border-b border-gray-800 hover:bg-gray-800/50">
                      <td className="px-4 py-2.5 text-gray-500 font-mono text-xs">{i + 1}</td>
                      <td className="px-4 py-2.5 text-white font-mono font-medium">{name}</td>
                      <td className="px-4 py-2.5">
                        <span className="px-2 py-0.5 bg-blue-900/30 text-blue-400 text-xs rounded font-mono">
                          {prop.range}
                        </span>
                      </td>
                      <td className="px-4 py-2.5">
                        {prop.required ? (
                          <span className="text-red-400 text-xs">YES</span>
                        ) : (
                          <span className="text-gray-500 text-xs">NO</span>
                        )}
                      </td>
                      <td className="px-4 py-2.5">
                        {prop.multi_valued ? (
                          <span className="text-purple-400 text-xs">YES</span>
                        ) : (
                          <span className="text-gray-600 text-xs">-</span>
                        )}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            ) : (
              <div className="text-gray-500 text-sm">该类暂无属性定义</div>
            )}

            {/* OntoQL example */}
            <div className="mt-6 p-4 bg-gray-800/50 rounded-lg border border-gray-700">
              <h4 className="text-gray-400 text-xs uppercase mb-2">查询示例</h4>
              <code className="text-green-400 text-sm font-mono">
                SELECT * FROM {activeClass.name} LIMIT 10
              </code>
            </div>
          </div>
        ) : (
          <div className="flex items-center justify-center h-full text-gray-500">
            <div className="text-center">
              <p className="text-2xl mb-2">📦</p>
              <p>选择左侧本体和类查看详情</p>
            </div>
          </div>
        )}
      </div>
    </div>
  )
}

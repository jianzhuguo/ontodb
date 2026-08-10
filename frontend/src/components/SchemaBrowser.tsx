import { useState, useEffect } from 'react'

interface ColumnInfo {
  name: string
  type: string
  nullable?: boolean
}

interface TableInfo {
  name: string
  columns: ColumnInfo[]
}

export function SchemaBrowser() {
  const [tables, setTables] = useState<TableInfo[]>([])
  const [selectedTable, setSelectedTable] = useState<TableInfo | null>(null)
  const [loading, setLoading] = useState(true)

  useEffect(() => {
    async function loadSchema() {
      try {
        const resp = await fetch('/api/schema')
        const data = await resp.json()
        const tableList = (data.data?.tables || []).map((t: { name: string; columns?: ColumnInfo[] }) => ({
          name: t.name,
          columns: t.columns || [],
        }))
        setTables(tableList)
      } catch (err) {
        console.error('Failed to load schema:', err)
      } finally {
        setLoading(false)
      }
    }
    loadSchema()
  }, [])

  if (loading) {
    return (
      <div className="flex items-center justify-center h-full text-gray-500">
        加载中...
      </div>
    )
  }

  return (
    <div className="flex h-full">
      {/* Table list */}
      <div className="w-56 border-r border-gray-800 bg-gray-900 overflow-y-auto">
        <div className="p-3 border-b border-gray-800">
          <h3 className="text-white font-medium text-sm">Schema 浏览</h3>
          <p className="text-gray-500 text-xs mt-1">{tables.length} 个表</p>
        </div>
        <div className="py-1">
          {tables.map((table) => (
            <button
              key={table.name}
              onClick={() => setSelectedTable(table)}
              className={`w-full text-left px-3 py-2 text-sm transition-colors flex items-center gap-2 ${
                selectedTable?.name === table.name
                  ? 'bg-blue-600/20 text-blue-400'
                  : 'text-gray-300 hover:bg-gray-800'
              }`}
            >
              <span className="text-gray-500">📋</span>
              <span>{table.name}</span>
              <span className="ml-auto text-xs text-gray-500">{table.columns.length}</span>
            </button>
          ))}
        </div>
      </div>

      {/* Column details */}
      <div className="flex-1 overflow-y-auto">
        {selectedTable ? (
          <div className="p-4">
            <div className="flex items-center gap-3 mb-4">
              <h2 className="text-white text-lg font-semibold">{selectedTable.name}</h2>
              <span className="px-2 py-0.5 bg-gray-800 text-gray-400 text-xs rounded">
                {selectedTable.columns.length} 列
              </span>
            </div>

            <table className="w-full text-sm">
              <thead>
                <tr className="bg-gray-800">
                  <th className="px-4 py-2.5 text-left text-gray-400 font-medium text-xs uppercase">#</th>
                  <th className="px-4 py-2.5 text-left text-gray-400 font-medium text-xs uppercase">列名</th>
                  <th className="px-4 py-2.5 text-left text-gray-400 font-medium text-xs uppercase">类型</th>
                  <th className="px-4 py-2.5 text-left text-gray-400 font-medium text-xs uppercase">可空</th>
                </tr>
              </thead>
              <tbody>
                {selectedTable.columns.map((col, i) => (
                  <tr key={col.name} className="border-b border-gray-800 hover:bg-gray-800/50">
                    <td className="px-4 py-2.5 text-gray-500 font-mono text-xs">{i + 1}</td>
                    <td className="px-4 py-2.5 text-white font-medium">{col.name}</td>
                    <td className="px-4 py-2.5">
                      <span className="px-2 py-0.5 bg-blue-900/30 text-blue-400 text-xs rounded font-mono">
                        {col.type}
                      </span>
                    </td>
                    <td className="px-4 py-2.5">
                      {col.nullable !== false ? (
                        <span className="text-yellow-400 text-xs">YES</span>
                      ) : (
                        <span className="text-gray-500 text-xs">NO</span>
                      )}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        ) : (
          <div className="flex items-center justify-center h-full text-gray-500">
            选择左侧表名查看详情
          </div>
        )}
      </div>
    </div>
  )
}

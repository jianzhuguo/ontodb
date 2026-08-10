import { useState, useEffect, useCallback } from 'react'

interface TableInfo {
  name: string
  columns: { name: string; type: string }[]
  row_count: number
}

export function DataBrowser() {
  const [tables, setTables] = useState<TableInfo[]>([])
  const [selectedTable, setSelectedTable] = useState<string | null>(null)
  const [rows, setRows] = useState<Record<string, unknown>[]>([])
  const [columns, setColumns] = useState<string[]>([])
  const [loading, setLoading] = useState(false)
  const [page, setPage] = useState(0)
  const pageSize = 50

  // Load tables
  useEffect(() => {
    async function loadTables() {
      try {
        const resp = await fetch('/api/schema')
        const data = await resp.json()
        const tableList = (data.data?.tables || []).map((t: { name: string; columns?: { name: string; type: string }[] }) => ({
          name: t.name,
          columns: t.columns || [],
          row_count: 0,
        }))
        setTables(tableList)
      } catch (err) {
        console.error('Failed to load schema:', err)
      }
    }
    loadTables()
  }, [])

  // Load table data
  const loadTableData = useCallback(async (tableName: string, pageNum: number) => {
    setLoading(true)
    try {
      const offset = pageNum * pageSize
      const resp = await fetch('/api/query', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          query: `SELECT * FROM ${tableName} LIMIT ${pageSize} OFFSET ${offset}`,
        }),
      })
      const data = await resp.json()
      const resultRows = data.data || []
      const cols = resultRows.length > 0 ? Object.keys(resultRows[0]) : []
      setColumns(cols)
      setRows(resultRows)
    } catch (err) {
      console.error('Failed to load data:', err)
    } finally {
      setLoading(false)
    }
  }, [])

  // Select table
  const handleSelectTable = (tableName: string) => {
    setSelectedTable(tableName)
    setPage(0)
    loadTableData(tableName, 0)
  }

  // Pagination
  const handleNextPage = () => {
    if (selectedTable && rows.length === pageSize) {
      const nextPage = page + 1
      setPage(nextPage)
      loadTableData(selectedTable, nextPage)
    }
  }

  const handlePrevPage = () => {
    if (selectedTable && page > 0) {
      const prevPage = page - 1
      setPage(prevPage)
      loadTableData(selectedTable, prevPage)
    }
  }

  return (
    <div className="flex h-full">
      {/* Table list */}
      <div className="w-48 border-r border-gray-700 overflow-y-auto bg-gray-900">
        <div className="p-2 text-xs text-gray-400 font-medium uppercase">Tables</div>
        {tables.map((table) => (
          <button
            key={table.name}
            onClick={() => handleSelectTable(table.name)}
            className={`w-full text-left px-3 py-2 text-sm transition-colors ${
              selectedTable === table.name
                ? 'bg-blue-600/20 text-blue-400 border-l-2 border-blue-500'
                : 'text-gray-300 hover:bg-gray-800'
            }`}
          >
            {table.name}
          </button>
        ))}
        {tables.length === 0 && (
          <div className="px-3 py-2 text-gray-500 text-sm">暂无表</div>
        )}
      </div>

      {/* Data view */}
      <div className="flex-1 flex flex-col min-w-0">
        {/* Header */}
        <div className="flex items-center gap-2 p-3 border-b border-gray-700 bg-gray-800">
          <span className="text-white font-medium">{selectedTable || '选择表'}</span>
          {selectedTable && (
            <>
              <span className="text-gray-400 text-sm">· {rows.length} 行</span>
              <div className="flex-1" />
              <button
                onClick={handlePrevPage}
                disabled={page === 0}
                className="px-2 py-1 text-sm bg-gray-700 hover:bg-gray-600 disabled:opacity-50 text-gray-300 rounded"
              >
                上一页
              </button>
              <span className="text-gray-400 text-sm">第 {page + 1} 页</span>
              <button
                onClick={handleNextPage}
                disabled={rows.length < pageSize}
                className="px-2 py-1 text-sm bg-gray-700 hover:bg-gray-600 disabled:opacity-50 text-gray-300 rounded"
              >
                下一页
              </button>
            </>
          )}
        </div>

        {/* Table */}
        <div className="flex-1 overflow-auto">
          {loading ? (
            <div className="flex items-center justify-center h-full text-gray-500">
              加载中...
            </div>
          ) : selectedTable && columns.length > 0 ? (
            <table className="w-full text-sm">
              <thead className="sticky top-0">
                <tr className="bg-gray-800">
                  <th className="px-3 py-2 text-left text-gray-400 font-medium border-b border-gray-700 w-12">#</th>
                  {columns.map((col) => (
                    <th key={col} className="px-3 py-2 text-left text-gray-300 font-medium border-b border-gray-700">
                      {col}
                    </th>
                  ))}
                </tr>
              </thead>
              <tbody>
                {rows.map((row, i) => (
                  <tr key={i} className="hover:bg-gray-800/50 border-b border-gray-800">
                    <td className="px-3 py-2 text-gray-500 font-mono text-xs">
                      {page * pageSize + i + 1}
                    </td>
                    {columns.map((col) => (
                      <td key={col} className="px-3 py-2 text-gray-300 font-mono max-w-xs truncate">
                        {row[col] === null ? (
                          <span className="text-gray-500">NULL</span>
                        ) : typeof row[col] === 'object' ? (
                          <span className="text-yellow-400">{JSON.stringify(row[col])}</span>
                        ) : (
                          String(row[col])
                        )}
                      </td>
                    ))}
                  </tr>
                ))}
              </tbody>
            </table>
          ) : selectedTable ? (
            <div className="flex items-center justify-center h-full text-gray-500">
              表为空
            </div>
          ) : (
            <div className="flex items-center justify-center h-full text-gray-500">
              选择左侧表名查看数据
            </div>
          )}
        </div>
      </div>
    </div>
  )
}

import { useState, useCallback } from 'react'

interface QueryResult {
  columns: string[]
  rows: unknown[][]
  elapsed_ms: number
  rows_affected?: number
}

export function QueryConsole() {
  const [sql, setSql] = useState('SELECT * FROM users LIMIT 10')
  const [results, setResults] = useState<QueryResult | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [loading, setLoading] = useState(false)
  const [history, setHistory] = useState<string[]>([])

  const executeQuery = useCallback(async () => {
    if (!sql.trim()) return

    setLoading(true)
    setError(null)
    setResults(null)

    try {
      const resp = await fetch('/api/query', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ query: sql }),
      })

      const data = await resp.json()

      if (data.error) {
        setError(data.error)
      } else {
        const rows = data.data || []
        const columns = rows.length > 0 ? Object.keys(rows[0]) : []
        const values = rows.map((r: Record<string, unknown>) => columns.map(c => r[c]))

        setResults({
          columns,
          rows: values,
          elapsed_ms: data.elapsed_ms || 0,
          rows_affected: data.rows_affected,
        })

        // Add to history
        setHistory(prev => [sql, ...prev.filter(h => h !== sql)].slice(0, 20))
      }
    } catch (err) {
      setError(`Connection error: ${err}`)
    } finally {
      setLoading(false)
    }
  }, [sql])

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) {
      e.preventDefault()
      executeQuery()
    }
  }

  return (
    <div className="flex flex-col h-full">
      {/* Toolbar */}
      <div className="flex items-center gap-2 p-3 border-b border-gray-700">
        <button
          onClick={executeQuery}
          disabled={loading}
          className="px-4 py-1.5 bg-blue-600 hover:bg-blue-700 disabled:bg-gray-600 text-white text-sm rounded font-medium transition-colors"
        >
          {loading ? '执行中...' : '执行 (Ctrl+Enter)'}
        </button>
        <div className="flex-1" />
        <select
          className="bg-gray-800 text-gray-300 text-sm rounded px-2 py-1 border border-gray-700"
          onChange={(e) => setSql(e.target.value)}
          value=""
        >
          <option value="" disabled>历史查询</option>
          {history.map((h, i) => (
            <option key={i} value={h}>{h.slice(0, 60)}...</option>
          ))}
        </select>
      </div>

      {/* Editor */}
      <div className="flex-1 flex flex-col min-h-0">
        <textarea
          value={sql}
          onChange={(e) => setSql(e.target.value)}
          onKeyDown={handleKeyDown}
          className="flex-1 bg-gray-900 text-green-400 font-mono text-sm p-4 resize-none outline-none border-b border-gray-700"
          placeholder="输入 SQL 查询..."
          spellCheck={false}
        />

        {/* Results */}
        <div className="flex-1 overflow-auto">
          {error && (
            <div className="m-3 p-3 bg-red-900/30 border border-red-700 rounded text-red-400 text-sm">
              {error}
            </div>
          )}

          {results && (
            <div className="m-3">
              <div className="text-gray-400 text-xs mb-2">
                {results.rows.length} 行 · {results.elapsed_ms.toFixed(1)}ms
                {results.rows_affected !== undefined && ` · ${results.rows_affected} 行受影响`}
              </div>

              {results.columns.length > 0 && (
                <div className="overflow-x-auto">
                  <table className="w-full text-sm">
                    <thead>
                      <tr className="bg-gray-800">
                        {results.columns.map((col, i) => (
                          <th key={i} className="px-3 py-2 text-left text-gray-300 font-medium border-b border-gray-700">
                            {col}
                          </th>
                        ))}
                      </tr>
                    </thead>
                    <tbody>
                      {results.rows.map((row, i) => (
                        <tr key={i} className="hover:bg-gray-800/50 border-b border-gray-800">
                          {row.map((cell, j) => (
                            <td key={j} className="px-3 py-2 text-gray-300 font-mono">
                              {cell === null ? <span className="text-gray-500">NULL</span> : String(cell)}
                            </td>
                          ))}
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
              )}
            </div>
          )}

          {!error && !results && !loading && (
            <div className="flex items-center justify-center h-full text-gray-500 text-sm">
              按 Ctrl+Enter 执行查询
            </div>
          )}
        </div>
      </div>
    </div>
  )
}

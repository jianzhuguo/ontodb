import { useState, useCallback } from 'react'
import { useQueryHistory } from '../hooks/useSettings'

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
  const [showHistory, setShowHistory] = useState(false)
  const { history, addEntry, clearHistory } = useQueryHistory()

  const executeQuery = useCallback(async () => {
    if (!sql.trim()) return

    setLoading(true)
    setError(null)
    setResults(null)
    const startTime = performance.now()

    try {
      const resp = await fetch('/api/query', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ query: sql }),
      })

      const data = await resp.json()
      const elapsed = performance.now() - startTime

      if (data.error) {
        setError(data.error)
        addEntry({ sql, timestamp: Date.now(), elapsed_ms: elapsed, rows: 0, error: data.error })
      } else {
        const rows = data.data || []
        const columns = rows.length > 0 ? Object.keys(rows[0]) : []
        const values = rows.map((r: Record<string, unknown>) => columns.map(c => r[c]))

        const result: QueryResult = {
          columns,
          rows: values,
          elapsed_ms: data.elapsed_ms || elapsed,
          rows_affected: data.rows_affected,
        }
        setResults(result)
        addEntry({ sql, timestamp: Date.now(), elapsed_ms: result.elapsed_ms, rows: rows.length })
      }
    } catch (err) {
      const elapsed = performance.now() - startTime
      setError(`Connection error: ${err}`)
      addEntry({ sql, timestamp: Date.now(), elapsed_ms: elapsed, rows: 0, error: String(err) })
    } finally {
      setLoading(false)
    }
  }, [sql, addEntry])

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) {
      e.preventDefault()
      executeQuery()
    }
  }

  const handleSelectHistory = (historicalSql: string) => {
    setSql(historicalSql)
    setShowHistory(false)
  }

  return (
    <div className="flex h-full">
      {/* History sidebar */}
      {showHistory && (
        <div className="w-64 border-r border-gray-800 bg-gray-900">
          <div className="flex items-center justify-between p-3 border-b border-gray-800">
            <h3 className="text-white text-sm font-medium">查询历史</h3>
            <button
              onClick={() => setShowHistory(false)}
              className="text-gray-400 hover:text-white text-sm"
            >
              ✕
            </button>
          </div>
          <div className="overflow-y-auto" style={{ height: 'calc(100% - 45px)' }}>
            {history.length === 0 ? (
              <div className="p-4 text-gray-500 text-sm text-center">暂无历史</div>
            ) : (
              history.map((item, i) => (
                <button
                  key={i}
                  onClick={() => handleSelectHistory(item.sql)}
                  className="w-full text-left p-3 hover:bg-gray-800 border-b border-gray-800/50 transition-colors"
                >
                  <div className="text-gray-300 text-xs font-mono truncate">{item.sql}</div>
                  <div className="flex gap-2 mt-1 text-xs text-gray-500">
                    <span>{item.rows} 行</span>
                    <span>{item.elapsed_ms.toFixed(1)}ms</span>
                  </div>
                </button>
              ))
            )}
          </div>
        </div>
      )}

      {/* Main area */}
      <div className="flex-1 flex flex-col">
        {/* Toolbar */}
        <div className="flex items-center gap-2 p-3 border-b border-gray-800 bg-gray-900">
          <button
            onClick={executeQuery}
            disabled={loading}
            className="px-4 py-1.5 bg-blue-600 hover:bg-blue-700 disabled:bg-gray-600 text-white text-sm rounded font-medium transition-colors"
          >
            {loading ? '⏳ 执行中...' : '▶ 执行 (Ctrl+Enter)'}
          </button>
          <button
            onClick={() => setShowHistory(!showHistory)}
            className={`px-3 py-1.5 text-sm rounded transition-colors ${
              showHistory ? 'bg-gray-700 text-white' : 'bg-gray-800 text-gray-400 hover:text-white'
            }`}
          >
            📋 历史
          </button>
          <button
            onClick={() => { setResults(null); setError(null) }}
            className="px-3 py-1.5 bg-gray-800 text-gray-400 hover:text-white text-sm rounded transition-colors"
          >
            🗑 清空
          </button>
          <div className="flex-1" />
          {results && (
            <span className="text-gray-400 text-xs">
              {results.rows.length} 行 · {results.elapsed_ms.toFixed(1)}ms
            </span>
          )}
        </div>

        {/* Editor + Results */}
        <div className="flex-1 flex flex-col min-h-0">
          <textarea
            value={sql}
            onChange={(e) => setSql(e.target.value)}
            onKeyDown={handleKeyDown}
            className="h-40 bg-gray-950 text-green-400 font-mono text-sm p-4 resize-none outline-none border-b border-gray-800"
            placeholder="输入 SQL 查询..."
            spellCheck={false}
          />

          <div className="flex-1 overflow-auto">
            {error && (
              <div className="m-3 p-3 bg-red-900/30 border border-red-700 rounded text-red-400 text-sm">
                ❌ {error}
              </div>
            )}

            {results && results.columns.length > 0 && (
              <div className="overflow-x-auto">
                <table className="w-full text-sm">
                  <thead className="sticky top-0">
                    <tr className="bg-gray-800">
                      <th className="px-3 py-2 text-left text-gray-400 font-medium text-xs w-12">#</th>
                      {results.columns.map((col, i) => (
                        <th key={i} className="px-3 py-2 text-left text-gray-300 font-medium border-b border-gray-700">
                          {col}
                        </th>
                      ))}
                    </tr>
                  </thead>
                  <tbody>
                    {results.rows.map((row, i) => (
                      <tr key={i} className="hover:bg-gray-800/50 border-b border-gray-800/50">
                        <td className="px-3 py-2 text-gray-500 font-mono text-xs">{i + 1}</td>
                        {row.map((cell, j) => (
                          <td key={j} className="px-3 py-2 text-gray-300 font-mono">
                            {cell === null ? (
                              <span className="text-gray-500 italic">NULL</span>
                            ) : typeof cell === 'boolean' ? (
                              <span className={cell ? 'text-green-400' : 'text-red-400'}>
                                {String(cell)}
                              </span>
                            ) : typeof cell === 'number' ? (
                              <span className="text-yellow-300">{cell}</span>
                            ) : (
                              String(cell)
                            )}
                          </td>
                        ))}
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            )}

            {!error && !results && !loading && (
              <div className="flex flex-col items-center justify-center h-full text-gray-500 gap-2">
                <div className="text-4xl">💻</div>
                <div className="text-sm">按 Ctrl+Enter 执行查询</div>
                <div className="text-xs text-gray-600">支持 SQL、SPARQL、GRAPH TRAVERSE</div>
              </div>
            )}
          </div>
        </div>
      </div>
    </div>
  )
}

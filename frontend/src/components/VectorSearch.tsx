import { useState, useCallback } from 'react'

interface SearchResult {
  [key: string]: unknown
  _score?: number
}

export function VectorSearch() {
  const [tableName, setTableName] = useState('documents')
  const [columnName, setColumnName] = useState('embedding')
  const [dimensions, setDimensions] = useState(128)
  const [topK, setTopK] = useState(10)
  const [vectorInput, setVectorInput] = useState('')
  const [filter, setFilter] = useState('')
  const [results, setResults] = useState<SearchResult[]>([])
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)

  // Generate random vector
  const generateRandomVector = useCallback(() => {
    const vec = Array.from({ length: dimensions }, () => (Math.random() * 2 - 1).toFixed(4))
    setVectorInput(vec.join(', '))
  }, [dimensions])

  // Execute search
  const handleSearch = useCallback(async () => {
    setLoading(true)
    setError(null)
    setResults([])

    try {
      // Parse vector
      const vector = vectorInput.split(',').map(s => parseFloat(s.trim())).filter(n => !isNaN(n))
      if (vector.length === 0) {
        setError('请输入有效的向量')
        return
      }

      const body: Record<string, unknown> = {
        class: tableName,
        column: columnName,
        query_vector: vector,
        top_k: topK,
      }
      if (filter.trim()) {
        body.filter = filter
      }

      const resp = await fetch('/api/vector/search', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body),
      })

      const data = await resp.json()
      if (data.error) {
        setError(data.error)
      } else {
        setResults(data.data || [])
      }
    } catch (err) {
      setError(`搜索失败: ${err}`)
    } finally {
      setLoading(false)
    }
  }, [tableName, columnName, topK, vectorInput, filter])

  return (
    <div className="flex flex-col h-full">
      {/* Config */}
      <div className="p-4 border-b border-gray-700 bg-gray-800 space-y-3">
        <div className="grid grid-cols-3 gap-3">
          <div>
            <label className="block text-xs text-gray-400 mb-1">表名</label>
            <input
              value={tableName}
              onChange={(e) => setTableName(e.target.value)}
              className="w-full bg-gray-900 text-white text-sm rounded px-3 py-1.5 border border-gray-700 outline-none focus:border-blue-500"
            />
          </div>
          <div>
            <label className="block text-xs text-gray-400 mb-1">向量列</label>
            <input
              value={columnName}
              onChange={(e) => setColumnName(e.target.value)}
              className="w-full bg-gray-900 text-white text-sm rounded px-3 py-1.5 border border-gray-700 outline-none focus:border-blue-500"
            />
          </div>
          <div>
            <label className="block text-xs text-gray-400 mb-1">Top K</label>
            <input
              type="number"
              value={topK}
              onChange={(e) => setTopK(parseInt(e.target.value) || 10)}
              className="w-full bg-gray-900 text-white text-sm rounded px-3 py-1.5 border border-gray-700 outline-none focus:border-blue-500"
            />
          </div>
        </div>

        <div>
          <label className="block text-xs text-gray-400 mb-1">查询向量 ({dimensions}维)</label>
          <textarea
            value={vectorInput}
            onChange={(e) => setVectorInput(e.target.value)}
            className="w-full bg-gray-900 text-green-400 font-mono text-xs rounded px-3 py-2 border border-gray-700 outline-none focus:border-blue-500 h-20 resize-none"
            placeholder="0.1, 0.2, 0.3, ..."
          />
          <div className="flex gap-2 mt-1">
            <button
              onClick={generateRandomVector}
              className="px-3 py-1 text-xs bg-gray-700 hover:bg-gray-600 text-gray-300 rounded"
            >
              随机生成
            </button>
            <button
              onClick={() => setVectorInput('')}
              className="px-3 py-1 text-xs bg-gray-700 hover:bg-gray-600 text-gray-300 rounded"
            >
              清空
            </button>
          </div>
        </div>

        <div>
          <label className="block text-xs text-gray-400 mb-1">过滤条件 (可选)</label>
          <input
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
            className="w-full bg-gray-900 text-white text-sm rounded px-3 py-1.5 border border-gray-700 outline-none focus:border-blue-500"
            placeholder='category = "技术"'
          />
        </div>

        <button
          onClick={handleSearch}
          disabled={loading || !vectorInput.trim()}
          className="w-full px-4 py-2 bg-blue-600 hover:bg-blue-700 disabled:bg-gray-600 text-white text-sm rounded font-medium transition-colors"
        >
          {loading ? '搜索中...' : '向量搜索'}
        </button>
      </div>

      {/* Results */}
      <div className="flex-1 overflow-auto p-4">
        {error && (
          <div className="p-3 bg-red-900/30 border border-red-700 rounded text-red-400 text-sm mb-3">
            {error}
          </div>
        )}

        {results.length > 0 && (
          <div className="space-y-2">
            <div className="text-gray-400 text-sm mb-3">
              找到 {results.length} 个结果
            </div>
            {results.map((result, i) => (
              <div key={i} className="bg-gray-800 rounded-lg p-3 border border-gray-700">
                <div className="flex items-center gap-2 mb-2">
                  <span className="text-blue-400 font-mono text-sm">#{i + 1}</span>
                  {result._score !== undefined && (
                    <span className="text-green-400 text-xs">
                      相似度: {(result._score * 100).toFixed(1)}%
                    </span>
                  )}
                </div>
                <div className="space-y-1">
                  {Object.entries(result)
                    .filter(([k]) => k !== '_score')
                    .map(([key, value]) => (
                      <div key={key} className="flex gap-2 text-sm">
                        <span className="text-gray-400 min-w-[100px]">{key}:</span>
                        <span className="text-gray-200 font-mono break-all">
                          {value === null ? (
                            <span className="text-gray-500">NULL</span>
                          ) : typeof value === 'object' ? (
                            JSON.stringify(value)
                          ) : (
                            String(value)
                          )}
                        </span>
                      </div>
                    ))}
                </div>
              </div>
            ))}
          </div>
        )}

        {!error && results.length === 0 && !loading && (
          <div className="flex items-center justify-center h-full text-gray-500 text-sm">
            输入向量并点击搜索
          </div>
        )}
      </div>
    </div>
  )
}

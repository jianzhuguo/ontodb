package io.ontodb;

import com.google.gson.Gson;
import com.google.gson.reflect.TypeToken;

import java.lang.reflect.Type;
import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.time.Duration;
import java.util.*;

/**
 * OntoDB client for Java 11+.
 *
 * <pre>{@code
 * OntoDBClient client = new OntoDBClient("http://localhost:7912", "your-key");
 *
 * // SQL
 * List<Map<String, Object>> rows = client.query("SELECT * FROM users");
 *
 * // Vector search
 * List<Map<String, Object>> results = client.vectorSearch(
 *     "documents", "embedding", new double[]{0.1, 0.2, 0.3}, 5
 * );
 *
 * // Close
 * client.close();
 * }</pre>
 */
public class OntoDBClient implements AutoCloseable {

    private static final Gson GSON = new Gson();
    private static final Type MAP_TYPE = new TypeToken<Map<String, Object>>(){}.getType();
    private static final Type LIST_MAP_TYPE = new TypeToken<List<Map<String, Object>>>(){}.getType();

    private final String baseUrl;
    private final String apiKey;
    private final HttpClient httpClient;
    private final int maxRetries;

    /**
     * Create a new OntoDB client.
     *
     * @param baseUrl server URL (e.g., "http://localhost:7912")
     */
    public OntoDBClient(String baseUrl) {
        this(baseUrl, null, 30, 3);
    }

    /**
     * Create a new OntoDB client with API key.
     *
     * @param baseUrl server URL
     * @param apiKey  API key for authentication
     */
    public OntoDBClient(String baseUrl, String apiKey) {
        this(baseUrl, apiKey, 30, 3);
    }

    /**
     * Create a new OntoDB client with full options.
     *
     * @param baseUrl    server URL
     * @param apiKey     API key (nullable)
     * @param timeoutSec timeout in seconds
     * @param maxRetries max retries on failure
     */
    public OntoDBClient(String baseUrl, String apiKey, int timeoutSec, int maxRetries) {
        this.baseUrl = baseUrl.replaceAll("/+$", "");
        this.apiKey = apiKey;
        this.maxRetries = maxRetries;
        this.httpClient = HttpClient.newBuilder()
                .connectTimeout(Duration.ofSeconds(timeoutSec))
                .build();
    }

    // ──────────────────────────────────────────────
    // SQL Queries
    // ──────────────────────────────────────────────

    /**
     * Execute a SQL query and return results.
     *
     * @param sql SQL query
     * @return list of row maps
     * @throws OntoDBException on query error
     */
    public List<Map<String, Object>> query(String sql) throws OntoDBException {
        Map<String, String> body = new HashMap<>();
        body.put("query", sql);
        ApiResponse resp = doRequest("POST", "/api/query", body);
        if (resp.error != null && !resp.error.isEmpty()) {
            throw new QueryException(resp.error);
        }
        return resp.data != null ? GSON.fromJson(GSON.toJson(resp.data), LIST_MAP_TYPE) : Collections.emptyList();
    }

    /**
     * Execute a SQL statement (INSERT/UPDATE/DELETE/DDL).
     *
     * @param sql SQL statement
     * @throws OntoDBException on error
     */
    public void execute(String sql) throws OntoDBException {
        Map<String, String> body = new HashMap<>();
        body.put("query", sql);
        ApiResponse resp = doRequest("POST", "/api/query", body);
        if (resp.error != null && !resp.error.isEmpty()) {
            throw new QueryException(resp.error);
        }
    }

    /**
     * Batch-insert multiple rows.
     *
     * @param table table name
     * @param rows  list of row maps
     * @throws OntoDBException on error
     */
    public void insertMany(String table, List<Map<String, Object>> rows) throws OntoDBException {
        if (rows == null || rows.isEmpty()) return;

        Set<String> columns = rows.get(0).keySet();
        String colsStr = String.join(", ", columns);

        StringBuilder values = new StringBuilder();
        for (int i = 0; i < rows.size(); i++) {
            if (i > 0) values.append(", ");
            values.append("(");
            int j = 0;
            for (String col : columns) {
                if (j > 0) values.append(", ");
                Object v = rows.get(i).get(col);
                if (v == null) {
                    values.append("NULL");
                } else if (v instanceof String) {
                    values.append("'").append(((String) v).replace("'", "''")).append("'");
                } else if (v instanceof Boolean) {
                    values.append((Boolean) v ? "TRUE" : "FALSE");
                } else {
                    values.append(v);
                }
                j++;
            }
            values.append(")");
        }

        String sql = String.format("BATCH INSERT INTO %s (%s) VALUES %s", table, colsStr, values);
        execute(sql);
    }

    // ──────────────────────────────────────────────
    // Vector Search
    // ──────────────────────────────────────────────

    /**
     * Vector similarity search.
     *
     * @param table  table name
     * @param column vector column name
     * @param vector query vector
     * @param topK   number of results
     * @return list of matching rows
     * @throws OntoDBException on error
     */
    public List<Map<String, Object>> vectorSearch(String table, String column, double[] vector, int topK) throws OntoDBException {
        return vectorSearch(table, column, vector, topK, null);
    }

    /**
     * Vector similarity search with filter.
     */
    public List<Map<String, Object>> vectorSearch(String table, String column, double[] vector, int topK, String filter) throws OntoDBException {
        Map<String, Object> body = new HashMap<>();
        body.put("class", table);
        body.put("column", column);
        body.put("query_vector", vector);
        body.put("top_k", topK);
        if (filter != null) body.put("filter", filter);

        ApiResponse resp = doRequest("POST", "/api/vector/search", body);
        if (resp.error != null && !resp.error.isEmpty()) {
            throw new QueryException(resp.error);
        }
        return resp.data != null ? GSON.fromJson(GSON.toJson(resp.data), LIST_MAP_TYPE) : Collections.emptyList();
    }

    // ──────────────────────────────────────────────
    // SPARQL
    // ──────────────────────────────────────────────

    /**
     * Execute a SPARQL query.
     *
     * @param query SPARQL query
     * @return list of result bindings
     * @throws OntoDBException on error
     */
    public List<Map<String, Object>> sparql(String query) throws OntoDBException {
        Map<String, String> body = new HashMap<>();
        body.put("query", query);
        ApiResponse resp = doRequest("POST", "/api/sparql", body);
        if (resp.error != null && !resp.error.isEmpty()) {
            throw new QueryException(resp.error);
        }
        return resp.data != null ? GSON.fromJson(GSON.toJson(resp.data), LIST_MAP_TYPE) : Collections.emptyList();
    }

    // ──────────────────────────────────────────────
    // Graph Operations
    // ──────────────────────────────────────────────

    /**
     * Traverse the graph from a starting vertex.
     *
     * @param startId   starting vertex ID
     * @param direction "in", "out", or "both"
     * @param depth     max traversal depth
     * @return traversal result with vertices and edges
     * @throws OntoDBException on error
     */
    public Map<String, Object> graphTraverse(String startId, String direction, int depth) throws OntoDBException {
        Map<String, Object> body = new HashMap<>();
        body.put("start", startId);
        body.put("direction", direction);
        body.put("max_depth", depth);
        body.put("algorithm", "bfs");

        ApiResponse resp = doRequest("POST", "/api/graph/traverse", body);
        if (resp.error != null && !resp.error.isEmpty()) {
            throw new QueryException(resp.error);
        }
        return resp.data != null ? GSON.fromJson(GSON.toJson(resp.data), MAP_TYPE) : Collections.emptyMap();
    }

    /**
     * Find shortest path between two vertices.
     *
     * @param fromId source vertex ID
     * @param toId   target vertex ID
     * @return list of vertex IDs on the path
     * @throws OntoDBException on error
     */
    @SuppressWarnings("unchecked")
    public List<String> graphShortestPath(String fromId, String toId) throws OntoDBException {
        Map<String, String> body = new HashMap<>();
        body.put("from", fromId);
        body.put("to", toId);

        ApiResponse resp = doRequest("POST", "/api/graph/shortest-path", body);
        if (resp.error != null && !resp.error.isEmpty()) {
            throw new QueryException(resp.error);
        }
        if (resp.data != null) {
            Map<String, Object> data = GSON.fromJson(GSON.toJson(resp.data), MAP_TYPE);
            Object path = data.get("path");
            if (path instanceof List) {
                return (List<String>) path;
            }
        }
        return Collections.emptyList();
    }

    // ──────────────────────────────────────────────
    // Health & Schema
    // ──────────────────────────────────────────────

    /**
     * Check server health.
     *
     * @return health status map
     * @throws OntoDBException on error
     */
    public Map<String, Object> health() throws OntoDBException {
        ApiResponse resp = doRequest("GET", "/api/health", null);
        return resp.data != null ? GSON.fromJson(GSON.toJson(resp.data), MAP_TYPE) : Collections.emptyMap();
    }

    /**
     * Check if server is ready.
     *
     * @return true if ready
     */
    public boolean isReady() {
        try {
            Map<String, Object> h = health();
            return "ok".equals(h.get("status"));
        } catch (Exception e) {
            return false;
        }
    }

    /**
     * Get database schema.
     *
     * @return schema map
     * @throws OntoDBException on error
     */
    public Map<String, Object> schema() throws OntoDBException {
        ApiResponse resp = doRequest("GET", "/api/schema", null);
        return resp.data != null ? GSON.fromJson(GSON.toJson(resp.data), MAP_TYPE) : Collections.emptyMap();
    }

    // ──────────────────────────────────────────────
    // Backup
    // ──────────────────────────────────────────────

    /**
     * Create a full backup.
     *
     * @param path backup file path on server
     * @throws OntoDBException on error
     */
    public void backup(String path) throws OntoDBException {
        Map<String, String> body = new HashMap<>();
        body.put("path", path);
        ApiResponse resp = doRequest("POST", "/api/backup", body);
        if (resp.error != null && !resp.error.isEmpty()) {
            throw new QueryException(resp.error);
        }
    }

    /**
     * Restore from backup.
     *
     * @param path backup file path on server
     * @throws OntoDBException on error
     */
    public void restore(String path) throws OntoDBException {
        Map<String, String> body = new HashMap<>();
        body.put("path", path);
        ApiResponse resp = doRequest("POST", "/api/restore", body);
        if (resp.error != null && !resp.error.isEmpty()) {
            throw new QueryException(resp.error);
        }
    }

    // ──────────────────────────────────────────────
    // Internal
    // ──────────────────────────────────────────────

    private ApiResponse doRequest(String method, String path, Object body) throws OntoDBException {
        String url = baseUrl + path;
        Exception lastError = null;

        for (int attempt = 0; attempt <= maxRetries; attempt++) {
            try {
                HttpRequest.Builder builder = HttpRequest.newBuilder()
                        .uri(URI.create(url))
                        .timeout(Duration.ofSeconds(30));

                if (apiKey != null) {
                    builder.header("Authorization", "Bearer " + apiKey);
                }

                if ("POST".equals(method)) {
                    String json = body != null ? GSON.toJson(body) : "{}";
                    builder.header("Content-Type", "application/json")
                           .POST(HttpRequest.BodyPublishers.ofString(json));
                } else {
                    builder.GET();
                }

                HttpResponse<String> response = httpClient.send(builder.build(), HttpResponse.BodyHandlers.ofString());

                if (response.statusCode() == 401) {
                    throw new AuthenticationException("Invalid API key");
                }
                if (response.statusCode() == 429) {
                    throw new RateLimitException("Rate limit exceeded");
                }
                if (response.statusCode() >= 400) {
                    throw new QueryException("HTTP " + response.statusCode() + ": " + response.body());
                }

                return GSON.fromJson(response.body(), ApiResponse.class);

            } catch (OntoDBException e) {
                throw e;
            } catch (Exception e) {
                lastError = e;
                if (attempt < maxRetries) {
                    try { Thread.sleep(500L * (attempt + 1)); } catch (InterruptedException ignored) {}
                }
            }
        }

        throw new ConnectionException("Cannot connect to " + baseUrl + ": " + lastError);
    }

    @Override
    public void close() {
        // HttpClient doesn't require explicit close in Java 11+
    }

    @Override
    public String toString() {
        return String.format("OntoDBClient('%s')", baseUrl);
    }

    // ──────────────────────────────────────────────
    // Response model
    // ──────────────────────────────────────────────

    static class ApiResponse {
        Object data;
        String error;
        int rows_affected;
    }
}

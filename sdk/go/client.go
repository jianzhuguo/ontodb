// Package ontodb provides a Go client for the OntoDB semantic multi-modal database.
//
// Basic usage:
//
//	client, err := ontodb.New("http://localhost:7912", ontodb.WithAPIKey("your-key"))
//	if err != nil {
//	    log.Fatal(err)
//	}
//	defer client.Close()
//
//	// Execute SQL
//	rows, err := client.Query("SELECT * FROM users LIMIT 10")
//	for _, row := range rows {
//	    fmt.Println(row["name"], row["age"])
//	}
package ontodb

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"strings"
	"time"
)

// Version is the SDK version.
const Version = "0.6.1"

// Client is the OntoDB client.
type Client struct {
	baseURL    string
	apiKey     string
	httpClient *http.Client
	maxRetries int
}

// Option configures the client.
type Option func(*Client)

// WithAPIKey sets the API key for authentication.
func WithAPIKey(key string) Option {
	return func(c *Client) { c.apiKey = key }
}

// WithTimeout sets the HTTP timeout.
func WithTimeout(d time.Duration) Option {
	return func(c *Client) { c.httpClient.Timeout = d }
}

// WithMaxRetries sets the maximum number of retries.
func WithMaxRetries(n int) Option {
	return func(c *Client) { c.maxRetries = n }
}

// WithHTTPClient sets a custom HTTP client.
func WithHTTPClient(hc *http.Client) Option {
	return func(c *Client) { c.httpClient = hc }
}

// New creates a new OntoDB client.
func New(baseURL string, opts ...Option) (*Client, error) {
	c := &Client{
		baseURL: strings.TrimRight(baseURL, "/"),
		httpClient: &http.Client{
			Timeout: 30 * time.Second,
		},
		maxRetries: 3,
	}
	for _, opt := range opts {
		opt(c)
	}
	return c, nil
}

// Close closes the client (no-op for HTTP client).
func (c *Client) Close() error {
	return nil
}

// apiResponse is the standard API response.
type apiResponse struct {
	Data         json.RawMessage `json:"data"`
	Error        string          `json:"error"`
	RowsAffected int             `json:"rows_affected"`
}

// doRequest sends an HTTP request with retry logic.
func (c *Client) doRequest(ctx context.Context, method, path string, body interface{}) (*apiResponse, error) {
	url := c.baseURL + path
	var lastErr error

	for attempt := 0; attempt <= c.maxRetries; attempt++ {
		var bodyReader io.Reader
		if body != nil {
			jsonBody, err := json.Marshal(body)
			if err != nil {
				return nil, fmt.Errorf("marshal request: %w", err)
			}
			bodyReader = bytes.NewReader(jsonBody)
		}

		req, err := http.NewRequestWithContext(ctx, method, url, bodyReader)
		if err != nil {
			return nil, fmt.Errorf("create request: %w", err)
		}
		req.Header.Set("Content-Type", "application/json")
		if c.apiKey != "" {
			req.Header.Set("Authorization", "Bearer "+c.apiKey)
		}

		resp, err := c.httpClient.Do(req)
		if err != nil {
			lastErr = &ConnectionError{Err: err}
			if attempt < c.maxRetries {
				time.Sleep(time.Duration(500*(attempt+1)) * time.Millisecond)
				continue
			}
			return nil, lastErr
		}

		respBody, err := io.ReadAll(resp.Body)
		resp.Body.Close()
		if err != nil {
			lastErr = &ConnectionError{Err: err}
			continue
		}

		if resp.StatusCode == 401 {
			return nil, &AuthenticationError{Message: "invalid API key"}
		}
		if resp.StatusCode == 429 {
			return nil, &RateLimitError{Message: "rate limit exceeded"}
		}
		if resp.StatusCode >= 400 {
			var errResp apiResponse
			if json.Unmarshal(respBody, &errResp) == nil && errResp.Error != "" {
				return nil, &QueryError{Message: errResp.Error}
			}
			return nil, &QueryError{Message: fmt.Sprintf("HTTP %d: %s", resp.StatusCode, string(respBody))}
		}

		var apiResp apiResponse
		if err := json.Unmarshal(respBody, &apiResp); err != nil {
			return nil, fmt.Errorf("decode response: %w", err)
		}
		return &apiResp, nil
	}

	return nil, lastErr
}

// Query executes a SQL query and returns results.
func (c *Client) Query(query string) ([]map[string]interface{}, error) {
	return c.QueryContext(context.Background(), query)
}

// QueryContext executes a SQL query with context.
func (c *Client) QueryContext(ctx context.Context, query string) ([]map[string]interface{}, error) {
	resp, err := c.doRequest(ctx, http.MethodPost, "/api/query", map[string]string{"query": query})
	if err != nil {
		return nil, err
	}
	if resp.Error != "" {
		return nil, &QueryError{Message: resp.Error}
	}

	var rows []map[string]interface{}
	if len(resp.Data) > 0 {
		if err := json.Unmarshal(resp.Data, &rows); err != nil {
			return nil, fmt.Errorf("decode rows: %w", err)
		}
	}
	return rows, nil
}

// Execute executes a SQL statement (INSERT/UPDATE/DELETE/DDL).
func (c *Client) Execute(query string) error {
	return c.ExecuteContext(context.Background(), query)
}

// ExecuteContext executes a SQL statement with context.
func (c *Client) ExecuteContext(ctx context.Context, query string) error {
	resp, err := c.doRequest(ctx, http.MethodPost, "/api/query", map[string]string{"query": query})
	if err != nil {
		return err
	}
	if resp.Error != "" {
		return &QueryError{Message: resp.Error}
	}
	return nil
}

// InsertMany batch-inserts multiple rows.
func (c *Client) InsertMany(table string, rows []map[string]interface{}) error {
	return c.InsertManyContext(context.Background(), table, rows)
}

// InsertManyContext batch-inserts multiple rows with context.
func (c *Client) InsertManyContext(ctx context.Context, table string, rows []map[string]interface{}) error {
	if len(rows) == 0 {
		return nil
	}

	columns := make([]string, 0)
	for k := range rows[0] {
		columns = append(columns, k)
	}
	colsStr := strings.Join(columns, ", ")

	var values []string
	for _, row := range rows {
		var vals []string
		for _, col := range columns {
			v, ok := row[col]
			if !ok || v == nil {
				vals = append(vals, "NULL")
				continue
			}
			switch val := v.(type) {
			case string:
				vals = append(vals, "'"+strings.ReplaceAll(val, "'", "''")+"'")
			case bool:
				if val {
					vals = append(vals, "TRUE")
				} else {
					vals = append(vals, "FALSE")
				}
			default:
				vals = append(vals, fmt.Sprintf("%v", val))
			}
		}
		values = append(values, "("+strings.Join(vals, ", ")+")")
	}

	sql := fmt.Sprintf("BATCH INSERT INTO %s (%s) VALUES %s", table, colsStr, strings.Join(values, ", "))
	return c.ExecuteContext(ctx, sql)
}

// VectorSearch performs a vector similarity search.
func (c *Client) VectorSearch(table, column string, vector []float64, topK int) ([]map[string]interface{}, error) {
	return c.VectorSearchContext(context.Background(), table, column, vector, topK, "")
}

// VectorSearchContext performs a vector similarity search with context and optional filter.
func (c *Client) VectorSearchContext(ctx context.Context, table, column string, vector []float64, topK int, filter string) ([]map[string]interface{}, error) {
	body := map[string]interface{}{
		"class":        table,
		"column":       column,
		"query_vector": vector,
		"top_k":        topK,
	}
	if filter != "" {
		body["filter"] = filter
	}

	resp, err := c.doRequest(ctx, http.MethodPost, "/api/vector/search", body)
	if err != nil {
		return nil, err
	}
	if resp.Error != "" {
		return nil, &QueryError{Message: resp.Error}
	}

	var rows []map[string]interface{}
	if len(resp.Data) > 0 {
		json.Unmarshal(resp.Data, &rows)
	}
	return rows, nil
}

// Sparql executes a SPARQL query.
func (c *Client) Sparql(query string) ([]map[string]interface{}, error) {
	return c.SparqlContext(context.Background(), query)
}

// SparqlContext executes a SPARQL query with context.
func (c *Client) SparqlContext(ctx context.Context, query string) ([]map[string]interface{}, error) {
	resp, err := c.doRequest(ctx, http.MethodPost, "/api/sparql", map[string]string{"query": query})
	if err != nil {
		return nil, err
	}
	if resp.Error != "" {
		return nil, &QueryError{Message: resp.Error}
	}

	var rows []map[string]interface{}
	if len(resp.Data) > 0 {
		json.Unmarshal(resp.Data, &rows)
	}
	return rows, nil
}

// GraphTraverse traverses the graph from a starting vertex.
func (c *Client) GraphTraverse(startID string, direction string, depth int) (map[string]interface{}, error) {
	return c.GraphTraverseContext(context.Background(), startID, direction, depth, "")
}

// GraphTraverseContext traverses the graph with context and optional edge label filter.
func (c *Client) GraphTraverseContext(ctx context.Context, startID, direction string, depth int, edgeLabel string) (map[string]interface{}, error) {
	body := map[string]interface{}{
		"start":      startID,
		"direction":  direction,
		"max_depth":  depth,
		"algorithm":  "bfs",
	}
	if edgeLabel != "" {
		body["edge_label"] = edgeLabel
	}

	resp, err := c.doRequest(ctx, http.MethodPost, "/api/graph/traverse", body)
	if err != nil {
		return nil, err
	}
	if resp.Error != "" {
		return nil, &QueryError{Message: resp.Error}
	}

	var result map[string]interface{}
	if len(resp.Data) > 0 {
		json.Unmarshal(resp.Data, &result)
	}
	return result, nil
}

// GraphShortestPath finds the shortest path between two vertices.
func (c *Client) GraphShortestPath(fromID, toID string) ([]string, error) {
	return c.GraphShortestPathContext(context.Background(), fromID, toID)
}

// GraphShortestPathContext finds the shortest path with context.
func (c *Client) GraphShortestPathContext(ctx context.Context, fromID, toID string) ([]string, error) {
	resp, err := c.doRequest(ctx, http.MethodPost, "/api/graph/shortest-path", map[string]string{
		"from": fromID,
		"to":   toID,
	})
	if err != nil {
		return nil, err
	}
	if resp.Error != "" {
		return nil, &QueryError{Message: resp.Error}
	}

	var result struct {
		Path []string `json:"path"`
	}
	if len(resp.Data) > 0 {
		json.Unmarshal(resp.Data, &result)
	}
	return result.Path, nil
}

// Health checks server health.
func (c *Client) Health() (map[string]interface{}, error) {
	return c.HealthContext(context.Background())
}

// HealthContext checks server health with context.
func (c *Client) HealthContext(ctx context.Context) (map[string]interface{}, error) {
	resp, err := c.doRequest(ctx, http.MethodGet, "/api/health", nil)
	if err != nil {
		return nil, err
	}

	var result map[string]interface{}
	if len(resp.Data) > 0 {
		json.Unmarshal(resp.Data, &result)
	}
	return result, nil
}

// IsReady checks if the server is ready.
func (c *Client) IsReady() bool {
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()
	_, err := c.HealthContext(ctx)
	return err == nil
}

// Backup creates a full backup.
func (c *Client) Backup(path string) error {
	return c.BackupContext(context.Background(), path)
}

// BackupContext creates a full backup with context.
func (c *Client) BackupContext(ctx context.Context, path string) error {
	resp, err := c.doRequest(ctx, http.MethodPost, "/api/backup", map[string]string{"path": path})
	if err != nil {
		return err
	}
	if resp.Error != "" {
		return &QueryError{Message: resp.Error}
	}
	return nil
}

// Restore restores from a backup.
func (c *Client) Restore(path string) error {
	return c.RestoreContext(context.Background(), path)
}

// RestoreContext restores from a backup with context.
func (c *Client) RestoreContext(ctx context.Context, path string) error {
	resp, err := c.doRequest(ctx, http.MethodPost, "/api/restore", map[string]string{"path": path})
	if err != nil {
		return err
	}
	if resp.Error != "" {
		return &QueryError{Message: resp.Error}
	}
	return nil
}

// Schema returns database schema information.
func (c *Client) Schema() (map[string]interface{}, error) {
	return c.SchemaContext(context.Background())
}

// SchemaContext returns database schema with context.
func (c *Client) SchemaContext(ctx context.Context) (map[string]interface{}, error) {
	resp, err := c.doRequest(ctx, http.MethodGet, "/api/schema", nil)
	if err != nil {
		return nil, err
	}

	var result map[string]interface{}
	if len(resp.Data) > 0 {
		json.Unmarshal(resp.Data, &result)
	}
	return result, nil
}

// String returns a string representation of the client.
func (c *Client) String() string {
	return fmt.Sprintf("OntoDB('%s')", c.baseURL)
}

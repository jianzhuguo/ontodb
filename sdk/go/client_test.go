package ontodb

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"testing"
)

func TestNew(t *testing.T) {
	c, err := New("http://localhost:7912")
	if err != nil {
		t.Fatal(err)
	}
	if c.baseURL != "http://localhost:7912" {
		t.Errorf("expected baseURL http://localhost:7912, got %s", c.baseURL)
	}
}

func TestNewTrailingSlash(t *testing.T) {
	c, err := New("http://localhost:7912/")
	if err != nil {
		t.Fatal(err)
	}
	if c.baseURL != "http://localhost:7912" {
		t.Errorf("expected baseURL http://localhost:7912, got %s", c.baseURL)
	}
}

func TestWithAPIKey(t *testing.T) {
	c, _ := New("http://localhost:7912", WithAPIKey("test-key"))
	if c.apiKey != "test-key" {
		t.Errorf("expected apiKey test-key, got %s", c.apiKey)
	}
}

func TestString(t *testing.T) {
	c, _ := New("http://localhost:7912")
	if c.String() != "OntoDB('http://localhost:7912')" {
		t.Errorf("unexpected String(): %s", c.String())
	}
}

func TestIsReady(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		json.NewEncoder(w).Encode(map[string]string{"status": "ok"})
	}))
	defer srv.Close()

	c, _ := New(srv.URL)
	if !c.IsReady() {
		t.Error("expected IsReady() to return true")
	}
}

func TestIsReadyUnreachable(t *testing.T) {
	c, _ := New("http://127.0.0.1:1", WithMaxRetries(0), WithTimeout(100))
	if c.IsReady() {
		t.Error("expected IsReady() to return false")
	}
}

func TestQuery(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		json.NewEncoder(w).Encode(map[string]interface{}{
			"data": []map[string]interface{}{
				{"name": "Alice", "age": float64(30)},
			},
		})
	}))
	defer srv.Close()

	c, _ := New(srv.URL, WithMaxRetries(0))
	rows, err := c.Query("SELECT * FROM users")
	if err != nil {
		t.Fatal(err)
	}
	if len(rows) != 1 {
		t.Fatalf("expected 1 row, got %d", len(rows))
	}
	if rows[0]["name"] != "Alice" {
		t.Errorf("expected name Alice, got %v", rows[0]["name"])
	}
}

func TestQueryError(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		json.NewEncoder(w).Encode(map[string]string{"error": "table not found"})
	}))
	defer srv.Close()

	c, _ := New(srv.URL, WithMaxRetries(0))
	_, err := c.Query("SELECT * FROM nonexistent")
	if err == nil {
		t.Fatal("expected error")
	}
	if _, ok := err.(*QueryError); !ok {
		t.Errorf("expected QueryError, got %T", err)
	}
}

func TestExecute(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		json.NewEncoder(w).Encode(map[string]interface{}{"rows_affected": 1})
	}))
	defer srv.Close()

	c, _ := New(srv.URL, WithMaxRetries(0))
	err := c.Execute("INSERT INTO users (name) VALUES ('Bob')")
	if err != nil {
		t.Fatal(err)
	}
}

func TestAuthError(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(401)
		json.NewEncoder(w).Encode(map[string]string{"error": "unauthorized"})
	}))
	defer srv.Close()

	c, _ := New(srv.URL, WithMaxRetries(0))
	_, err := c.Query("SELECT 1")
	if err == nil {
		t.Fatal("expected error")
	}
	if _, ok := err.(*AuthenticationError); !ok {
		t.Errorf("expected AuthenticationError, got %T", err)
	}
}

func TestHealth(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		json.NewEncoder(w).Encode(map[string]string{"status": "ok"})
	}))
	defer srv.Close()

	c, _ := New(srv.URL, WithMaxRetries(0))
	h, err := c.Health()
	if err != nil {
		t.Fatal(err)
	}
	if h["status"] != "ok" {
		t.Errorf("expected status ok, got %v", h["status"])
	}
}

func TestInsertMany(t *testing.T) {
	var receivedSQL string
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		var body map[string]string
		json.NewDecoder(r.Body).Decode(&body)
		receivedSQL = body["query"]
		json.NewEncoder(w).Encode(map[string]interface{}{"rows_affected": 2})
	}))
	defer srv.Close()

	c, _ := New(srv.URL, WithMaxRetries(0))
	err := c.InsertMany("users", []map[string]interface{}{
		{"name": "Alice", "age": 30},
		{"name": "Bob", "age": 25},
	})
	if err != nil {
		t.Fatal(err)
	}
	if receivedSQL == "" {
		t.Error("expected SQL to be sent")
	}
}

func TestVectorSearch(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		json.NewEncoder(w).Encode(map[string]interface{}{
			"data": []map[string]interface{}{
				{"title": "doc1", "_score": 0.95},
			},
		})
	}))
	defer srv.Close()

	c, _ := New(srv.URL, WithMaxRetries(0))
	rows, err := c.VectorSearch("documents", "embedding", []float64{0.1, 0.2}, 5)
	if err != nil {
		t.Fatal(err)
	}
	if len(rows) != 1 {
		t.Fatalf("expected 1 result, got %d", len(rows))
	}
}

func TestSparql(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		json.NewEncoder(w).Encode(map[string]interface{}{
			"data": []map[string]interface{}{{"name": "Alice"}},
		})
	}))
	defer srv.Close()

	c, _ := New(srv.URL, WithMaxRetries(0))
	rows, err := c.Sparql("SELECT ?name WHERE { ?p ex:name ?name }")
	if err != nil {
		t.Fatal(err)
	}
	if len(rows) != 1 {
		t.Fatalf("expected 1 result, got %d", len(rows))
	}
}

func TestGraphTraverse(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		json.NewEncoder(w).Encode(map[string]interface{}{
			"data": map[string]interface{}{
				"vertices": []interface{}{map[string]string{"id": "P1"}},
				"edges":    []interface{}{},
			},
		})
	}))
	defer srv.Close()

	c, _ := New(srv.URL, WithMaxRetries(0))
	result, err := c.GraphTraverse("Person::1", "out", 3)
	if err != nil {
		t.Fatal(err)
	}
	if result == nil {
		t.Error("expected non-nil result")
	}
}

func TestBackup(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		json.NewEncoder(w).Encode(map[string]string{"path": "/backups/test"})
	}))
	defer srv.Close()

	c, _ := New(srv.URL, WithMaxRetries(0))
	err := c.Backup("/backups/test")
	if err != nil {
		t.Fatal(err)
	}
}

func TestContextCancellation(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		json.NewEncoder(w).Encode(map[string]interface{}{"data": []interface{}{}})
	}))
	defer srv.Close()

	c, _ := New(srv.URL, WithMaxRetries(0))
	ctx, cancel := context.WithCancel(context.Background())
	cancel() // Cancel immediately

	_, err := c.QueryContext(ctx, "SELECT 1")
	if err == nil {
		t.Error("expected error from cancelled context")
	}
}

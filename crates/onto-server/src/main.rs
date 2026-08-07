//! OntoDB Server - Main entry point.
//!
//! Supports three modes:
//! - Standalone REPL (interactive or stdin)
//! - TCP server (accepts multiple CLI connections)
//! - HTTP server (RESTful API with auth, rate limiting, and Prometheus metrics)

mod auth;
mod http;
mod metrics;
mod rate_limit;

use auth::{AuthConfig, AuthState, Permission};
use clap::Parser;
use onto_core::Result;
use onto_ontology::OntologyStore;
use onto_query::{QueryExecutor, QueryParser};
use onto_storage::{LsmEngine, StorageOptions};
use rate_limit::{RateLimitConfig, RateLimiter};
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

#[derive(Parser, Debug)]
#[command(name = "ontodb-server", about = "OntoDB - Ontology-driven semantic database")]
struct Args {
    /// Data directory path
    #[arg(short, long, default_value = "./ontodb_data", env = "STORAGE_DATA_DIR")]
    data_dir: PathBuf,

    /// MemTable size limit in bytes
    #[arg(short, long, default_value = "4194304", env = "STORAGE_MEMTABLE_SIZE")]
    memtable_size: usize,

    /// Run in interactive mode (REPL)
    #[arg(short, long)]
    interactive: bool,

    /// TCP listen address (enables TCP server mode)
    #[arg(short = 'l', long, default_value = "127.0.0.1:7913", env = "SERVER_LISTEN")]
    listen: String,

    /// HTTP listen address (enables HTTP API server mode)
    #[arg(long, env = "SERVER_HTTP")]
    http: Option<String>,

    /// Enable API key authentication
    #[arg(long, env = "AUTH_ENABLED")]
    auth: bool,

    /// API keys file path (JSON format)
    #[arg(long, env = "AUTH_API_KEYS_FILE")]
    api_keys_file: Option<PathBuf>,

    /// Default rate limit (requests per minute)
    #[arg(long, default_value = "60", env = "RATE_LIMIT_RPM")]
    rate_limit: u32,

    /// Rate limit burst size
    #[arg(long, default_value = "10", env = "RATE_LIMIT_BURST")]
    burst_size: u32,

    /// Disable rate limiting
    #[arg(long, env = "RATE_LIMIT_DISABLED")]
    no_rate_limit: bool,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let args = Args::parse();

    let options = StorageOptions {
        data_dir: args.data_dir,
        memtable_size_limit: args.memtable_size,
        ..Default::default()
    };

    println!("OntoDB v{}", env!("CARGO_PKG_VERSION"));
    println!("Data directory: {:?}", options.data_dir);

    let engine = Arc::new(RwLock::new(LsmEngine::open(options)?));
    let ontology_store = OntologyStore::new(Arc::clone(&engine));
    let executor = Arc::new(QueryExecutor::new(Arc::clone(&engine), ontology_store));

    if args.interactive {
        run_repl(&executor)?;
    } else {
        let metrics = Arc::new(metrics::Metrics::new());

        let rt = tokio::runtime::Runtime::new()
            .map_err(|e| onto_core::CoreError::Custom(format!("Failed to create tokio runtime: {}", e)))?;

        // Load auth configuration (needed if HTTP is enabled)
        let auth_config = if args.auth {
            load_auth_config(args.api_keys_file.as_deref())?
        } else {
            AuthConfig::default()
        };

        // Create rate limit config
        let rate_limit_config = RateLimitConfig {
            default_rpm: args.rate_limit,
            enabled: !args.no_rate_limit,
            burst_size: args.burst_size,
        };

        let has_http = args.http.is_some();
        let http_addr = args.http.unwrap_or_default();

        if has_http {
            // Run both HTTP and TCP servers concurrently
            rt.block_on(async {
                tokio::select! {
                    res = run_http_server(&http_addr, executor.clone(), auth_config, rate_limit_config, metrics.clone()) => res,
                    res = run_tcp_server(&args.listen, executor, metrics) => res,
                }
            })?;
        } else {
            // TCP only
            rt.block_on(run_tcp_server(&args.listen, executor, metrics))?;
        }
    }

    println!("Goodbye.");
    Ok(())
}

/// Loads authentication configuration from file or creates default.
fn load_auth_config(file_path: Option<&std::path::Path>) -> Result<AuthConfig> {
    if let Some(path) = file_path {
        let content = std::fs::read_to_string(path)
            .map_err(|e| onto_core::CoreError::Io(e))?;
        let config: AuthConfig = serde_json::from_str(&content)
            .map_err(|e| onto_core::CoreError::Custom(format!("Invalid auth config: {}", e)))?;
        Ok(config)
    } else {
        // Create default config with a warning
        eprintln!("WARNING: Authentication enabled without API keys file.");
        eprintln!("Use --api-keys-file to specify keys, or requests will be rejected.");
        Ok(AuthConfig {
            enabled: true,
            keys: Vec::new(),
            default_permission: Permission::ReadOnly,
        })
    }
}

/// Runs the HTTP API server using axum with authentication and rate limiting.
async fn run_http_server(
    addr: &str,
    executor: Arc<QueryExecutor>,
    auth_config: AuthConfig,
    rate_limit_config: RateLimitConfig,
    metrics: Arc<metrics::Metrics>,
) -> Result<()> {
    let state = http::AppState { executor, metrics };
    let auth_state = AuthState::new(&auth_config).with_metrics(state.metrics.clone());
    let rate_limiter = RateLimiter::new(rate_limit_config.clone());

    // Spawn background task to clean up stale rate limit buckets every 5 minutes
    let limiter_cleanup = rate_limiter.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(300));
        loop {
            interval.tick().await;
            limiter_cleanup.cleanup_stale(std::time::Duration::from_secs(600)).await;
        }
    });

    let app = http::build_router_with_auth(state, auth_state.clone(), rate_limiter);

    println!("HTTP API server listening on {}", addr);
    println!();
    println!("API endpoints:");
    println!("  GET  /api/health         - Health check");
    println!("  GET  /api/health/ready   - Readiness probe (Kubernetes)");
    println!("  GET  /api/health/live    - Liveness probe (Kubernetes)");
    println!("  GET  /metrics            - Prometheus metrics");
    println!("  GET  /api/metrics        - JSON metrics");
    println!("  POST /api/query          - Execute SQL query");
    println!("  POST /api/vector/search  - Vector similarity search");
    println!("  POST /api/hybrid/query   - Hybrid SQL + vector search");
    println!("  GET  /api/schema         - Schema introspection");
    println!();

    if auth_config.enabled {
        println!("Authentication: ENABLED");
        println!("  API keys: {}", auth_config.keys.len());
        println!("  Provide key via:");
        println!("    - Authorization: Bearer <key>");
        println!("    - X-API-Key: <key>");
        println!("    - ?api_key=<key> (query parameter)");
    } else {
        println!("Authentication: DISABLED");
    }

    if rate_limit_config.enabled {
        println!("Rate limiting: ENABLED");
        println!("  Default: {} requests/minute", rate_limit_config.default_rpm);
        println!("  Burst size: {}", rate_limit_config.burst_size);
    } else {
        println!("Rate limiting: DISABLED");
    }

    let listener = tokio::net::TcpListener::bind(addr).await
        .map_err(|e| onto_core::CoreError::Io(e))?;
    axum::serve(listener, app).await
        .map_err(|e| onto_core::CoreError::Custom(format!("HTTP server error: {}", e)))?;
    Ok(())
}

/// Runs the async TCP server, accepting client connections.
async fn run_tcp_server(addr: &str, executor: Arc<QueryExecutor>, metrics: Arc<metrics::Metrics>) -> Result<()> {
    let listener = TcpListener::bind(addr).await
        .map_err(|e| onto_core::CoreError::Io(e))?;

    println!("Listening on {}", addr);
    println!("Connect with: ontodb-cli {}", addr);

    loop {
        let (stream, _peer_addr) = listener.accept().await
            .map_err(|e| onto_core::CoreError::Io(e))?;

        let executor = Arc::clone(&executor);
        let metrics = Arc::clone(&metrics);
        metrics.tcp_connections_total.inc();
        metrics.tcp_connections_active.inc();

        tokio::spawn(async move {
            if let Err(e) = handle_client(stream, &executor, &metrics).await {
                eprintln!("Client error: {}", e);
            }
            metrics.tcp_connections_active.dec();
        });
    }
}

/// Handles a single client connection asynchronously.
///
/// Protocol:
/// - Client sends SQL queries, one per line (terminated by `\n`)
/// - Server executes and sends back formatted results
/// - Result is terminated by a null byte (`\0`) as end-of-message marker
/// - Errors are prefixed with `ERR: `
/// - Client sends `quit` or `exit` to disconnect
async fn handle_client(
    stream: tokio::net::TcpStream,
    executor: &QueryExecutor,
    metrics: &metrics::Metrics,
) -> Result<()> {
    let peer = stream.peer_addr().map(|a| a.to_string()).unwrap_or_default();
    println!("Client connected: {}", peer);

    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut line = String::new();

    loop {
        line.clear();
        let n = reader.read_line(&mut line).await
            .map_err(|e| onto_core::CoreError::Io(e))?;
        if n == 0 {
            break;
        }

        let input = line.trim();

        if input.is_empty() {
            writer.write_all(&[0]).await?;
            continue;
        }

        if input.eq_ignore_ascii_case("quit") || input.eq_ignore_ascii_case("exit") {
            break;
        }

        // Remove trailing semicolon
        let input = input.trim_end_matches(';').trim();

        // Execute query
        let start = std::time::Instant::now();
        let response = match QueryParser::parse(input) {
            Ok(ast) => {
                let query_type = match &ast {
                    onto_query::QueryAst::Select { .. } => "SELECT",
                    onto_query::QueryAst::Insert { .. } => "INSERT",
                    onto_query::QueryAst::Update { .. } => "UPDATE",
                    onto_query::QueryAst::Delete { .. } => "DELETE",
                    onto_query::QueryAst::VectorSearch { .. } => "VECTOR_SEARCH",
                    _ => "OTHER",
                };
                let result = if onto_query::QueryExecutor::is_read_only_query(&ast) {
                    executor.execute_read(&ast)
                } else {
                    executor.execute(&ast)
                };
                match result {
                    Ok(result) => {
                        let elapsed = start.elapsed().as_secs_f64();
                        metrics.record_query(query_type, elapsed, true);
                        result.format()
                    }
                    Err(e) => {
                        let elapsed = start.elapsed().as_secs_f64();
                        metrics.record_query(query_type, elapsed, false);
                        format!("ERR: {}", e)
                    }
                }
            }
            Err(e) => {
                metrics.record_parse_error();
                format!("ERR: Parse error: {}", e)
            }
        };

        // Send response + null terminator
        writer.write_all(response.as_bytes()).await?;
        writer.write_all(&[0]).await?;
        writer.flush().await?;
    }

    println!("Client disconnected: {}", peer);
    Ok(())
}

/// Runs the interactive REPL.
fn run_repl(executor: &QueryExecutor) -> Result<()> {
    println!("Interactive mode. Type 'quit' or 'exit' to leave.");
    println!("Type SQL or OntoDB queries. End statements with ';'.");
    println!();

    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let mut buffer = String::new();

    loop {
        print!("ontodb> ");
        stdout.flush()?;

        buffer.clear();
        if stdin.lock().read_line(&mut buffer)? == 0 {
            break;
        }

        let input = buffer.trim();
        if input.is_empty() {
            continue;
        }
        if input.eq_ignore_ascii_case("quit") || input.eq_ignore_ascii_case("exit") {
            break;
        }

        let input = input.trim_end_matches(';').trim();
        execute_and_print(executor, input);
    }

    Ok(())
}

fn execute_and_print(executor: &QueryExecutor, input: &str) {
    match QueryParser::parse(input) {
        Ok(ast) => {
            let result = if onto_query::QueryExecutor::is_read_only_query(&ast) {
                executor.execute_read(&ast)
            } else {
                executor.execute(&ast)
            };
            match result {
                Ok(result) => {
                    println!("{}", result.format());
                    println!();
                }
                Err(e) => {
                    eprintln!("Error: {}", e);
                }
            }
        }
        Err(e) => {
            eprintln!("Parse error: {}", e);
        }
    }
}

//! OntoDB Server - Main entry point.
//!
//! Supports three modes:
//! - Standalone REPL (interactive or stdin)
//! - TCP server (accepts multiple CLI connections)
//! - HTTP server (RESTful API with auth, rate limiting, and Prometheus metrics)

pub mod admin;
mod auth;
pub mod audit;
pub mod cdc;
mod http;
mod metrics;
pub mod mysqlwire;
pub mod pgwire;
mod rate_limit;
pub mod tls;

use auth::{AuthConfig, AuthState, Permission};
use clap::Parser;
use onto_core::Result;
use onto_enterprise::ProductTier;
use onto_ontology::OntologyStore;
use onto_query::{QueryExecutor, QueryParser};
use onto_storage::{LsmEngine, StorageOptions};
use rate_limit::{RateLimitConfig, RateLimiter};
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::sync::Arc;
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

    /// Enable query audit logging
    #[arg(long, env = "AUDIT_ENABLED")]
    audit: bool,

    /// Audit log directory
    #[arg(long, default_value = "audit_logs", env = "AUDIT_LOG_DIR")]
    audit_dir: PathBuf,

    /// PG wire protocol listen address (enables PostgreSQL compatibility)
    #[arg(long, env = "PGWIRE_LISTEN")]
    pgwire: Option<String>,

    /// MySQL protocol listen address (enables MySQL compatibility)
    #[arg(long, env = "MYSQL_LISTEN")]
    mysql: Option<String>,

    /// Raft node ID for distributed replication
    #[arg(long, env = "RAFT_NODE_ID")]
    raft_node_id: Option<u64>,

    /// Raft listen address for inter-node communication
    #[arg(long, env = "RAFT_LISTEN")]
    raft_listen: Option<String>,

    /// Raft peer nodes (format: id=addr,id=addr)
    #[arg(long, env = "RAFT_PEERS")]
    raft_peers: Option<String>,

    // === Enterprise Gov/Finance options ===
    /// Enable storage encryption (Gov/Finance edition only)
    #[arg(long, env = "ENCRYPTION_ENABLED")]
    encryption_enabled: bool,

    /// Master key file path for encryption (Gov/Finance edition only)
    #[arg(long, env = "ENCRYPTION_MASTER_KEY_FILE")]
    master_key_file: Option<PathBuf>,

    /// Master key from environment variable (Gov/Finance edition only)
    #[arg(long, env = "ENCRYPTION_MASTER_KEY_ENV")]
    master_key_env: Option<String>,

    /// Audit retention days (Gov/Finance edition only, default 180 per 等保2.0)
    #[arg(long, default_value = "180", env = "AUDIT_RETENTION_DAYS")]
    audit_retention_days: u64,

    /// Disable audit log compression (Gov/Finance edition only)
    #[arg(long, env = "AUDIT_NO_COMPRESS")]
    audit_no_compress: bool,

    /// Enterprise config file path (JSON format)
    #[arg(long, env = "ENTERPRISE_CONFIG_FILE")]
    enterprise_config: Option<PathBuf>,

    /// Allowed CORS origins (comma-separated). Use "*" for all origins (NOT recommended for production).
    #[arg(long, default_value = "", env = "CORS_ORIGINS")]
    cors_origins: String,

    /// TLS certificate file path (PEM format). Enables HTTPS when both cert and key are provided.
    #[arg(long, env = "TLS_CERT_PATH")]
    tls_cert: Option<PathBuf>,

    /// TLS private key file path (PEM format). Enables HTTPS when both cert and key are provided.
    #[arg(long, env = "TLS_KEY_PATH")]
    tls_key: Option<PathBuf>,

    /// Minimum TLS version: "1.2" or "1.3" (default: "1.2")
    #[arg(long, default_value = "1.2", env = "TLS_MIN_VERSION")]
    tls_min_version: String,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let args = Args::parse();

    let tier = onto_enterprise::current_tier();
    let features = onto_enterprise::enabled_features();

    // Initialize enterprise features first (before args are moved)
    let enterprise_config = build_enterprise_config(&args, tier);
    let enterprise_features = onto_enterprise::EnterpriseFeatures::init(&enterprise_config)
        .map_err(|e| onto_core::CoreError::Custom(format!("Enterprise features init failed: {}", e)))?;

    let options = StorageOptions {
        data_dir: args.data_dir,
        memtable_size_limit: args.memtable_size,
        ..Default::default()
    };

    println!("OntoDB v{}", env!("CARGO_PKG_VERSION"));
    println!("Edition: {:?}", tier);
    if !features.is_empty() {
        println!("Enterprise features: {}", features.join(", "));
    }
    println!("Data directory: {:?}", options.data_dir);

    // Log enterprise feature status
    #[cfg(feature = "encryption")]
    if enterprise_features.encryption.is_some() {
        println!("Storage encryption: ENABLED (AES-256-GCM)");
    }

    #[cfg(feature = "audit-retention")]
    if let Some(ref audit) = enterprise_features.audit_retention {
        let status = audit.status();
        println!("Audit retention: ENABLED ({} days)", status.retention_days);
    }

    let engine = Arc::new(LsmEngine::open(options)?);
    let ontology_store = OntologyStore::new(Arc::clone(&engine));
    let graph_store = Arc::new(onto_graph::GraphStore::new());
    let triple_store = Arc::new(onto_ontology::TripleStore::new(Arc::clone(&engine)));
    let executor = Arc::new(
        QueryExecutor::new(Arc::clone(&engine), ontology_store)
            .with_graph(graph_store.clone())
            .with_triple_store(triple_store)
    );

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

        // Create audit config
        let audit_config = audit::AuditConfig {
            enabled: args.audit,
            log_dir: args.audit_dir.clone(),
            ..Default::default()
        };

        if args.audit {
            println!("Audit logging enabled: {:?}", args.audit_dir);
        }

        let has_http = args.http.is_some();
        let http_addr = args.http.unwrap_or_default();

        let has_pgwire = args.pgwire.is_some();
        let pgwire_addr = args.pgwire.unwrap_or_default();

        let has_mysql = args.mysql.is_some();
        let mysql_addr = args.mysql.unwrap_or_default();

        let has_raft = args.raft_node_id.is_some() && args.raft_listen.is_some();

        if has_raft {
            println!("Raft node {} enabled, listening on {}", args.raft_node_id.unwrap(), args.raft_listen.as_deref().unwrap());
            if let Some(ref peers) = args.raft_peers {
                println!("  Peers: {}", peers);
            }
        }

        if has_http {
            rt.block_on(async {
                // Build TLS config if cert and key are provided
                let tls_config = match (&args.tls_cert, &args.tls_key) {
                    (Some(cert), Some(key)) => {
                        let min_version = match args.tls_min_version.as_str() {
                            "1.3" => tls::TlsVersion::Tls13,
                            _ => tls::TlsVersion::Tls12,
                        };
                        let config = tls::TlsConfig::new(cert.clone(), key.clone())
                            .with_min_version(min_version);
                        println!("TLS enabled: cert={:?}, key={:?}, min_version={}", cert, key, args.tls_min_version);
                        Some(config)
                    }
                    (Some(_), None) => {
                        eprintln!("Warning: --tls-cert provided without --tls-key, TLS disabled");
                        None
                    }
                    (None, Some(_)) => {
                        eprintln!("Warning: --tls-key provided without --tls-cert, TLS disabled");
                        None
                    }
                    _ => None,
                };

                let pg_auth = auth_config.clone();
                let mysql_auth = auth_config.clone();
                let mut futs: Vec<std::pin::Pin<Box<dyn std::future::Future<Output = std::result::Result<(), onto_core::CoreError>> + Send>>> = vec![
                    Box::pin(run_http_server(&http_addr, executor.clone(), auth_config, rate_limit_config, metrics.clone(), audit_config, args.raft_node_id, args.api_keys_file.clone(), args.cors_origins.clone(), tls_config, graph_store.clone())),
                    Box::pin(run_tcp_server(&args.listen, executor.clone(), metrics.clone())),
                ];

                if has_pgwire {
                    let exec = executor.clone();
                    let m = metrics.clone();
                    futs.push(Box::pin(async move {
                        pgwire::run_pgwire_server(&pgwire_addr, exec, m, pg_auth).await.map_err(|e| onto_core::CoreError::Custom(e.to_string()))
                    }));
                }

                if has_mysql {
                    let exec = executor.clone();
                    let m = metrics.clone();
                    futs.push(Box::pin(async move {
                        mysqlwire::run_mysql_server(&mysql_addr, exec, m, mysql_auth).await.map_err(|e| onto_core::CoreError::Custom(e.to_string()))
                    }));
                }

                if has_raft {
                    let raft_addr = args.raft_listen.clone().unwrap();
                    let node_id = args.raft_node_id.unwrap();
                    let peers = args.raft_peers.clone().unwrap_or_default();
                    futs.push(Box::pin(async move {
                        run_raft_node(node_id, &raft_addr, &peers).await
                    }));
                }

                // Run all servers concurrently. If any server fails, log the error
                // and initiate graceful shutdown of all servers. This is intentional:
                // for a database server, it's safer to shut down completely and let
                // the process manager (systemd/Docker) restart the whole service,
                // rather than running in a degraded state.
                let (res, idx, remaining) = futures::future::select_all(futs).await;
                let server_names = ["HTTP", "TCP", "PGWire", "MySQL", "Raft"];
                let failed_name = server_names.get(idx).unwrap_or(&"Unknown");
                match &res {
                    Ok(()) => tracing::info!("Server {} exited gracefully", failed_name),
                    Err(e) => tracing::error!("Server {} failed: {} — initiating shutdown", failed_name, e),
                }
                // Give remaining servers a brief window to finish in-flight requests
                let shutdown_timeout = std::time::Duration::from_secs(5);
                let _ = tokio::time::timeout(shutdown_timeout, async {
                    for fut in remaining {
                        let _ = fut.await;
                    }
                }).await;
                res
            })?;
        } else {
            rt.block_on(run_tcp_server(&args.listen, executor, metrics))?;
        }
    }

    println!("Goodbye.");
    Ok(())
}

/// Build enterprise configuration from command line arguments.
/// For Open Source edition, returns default config (features disabled).
/// For Gov/Finance edition, configures encryption and audit retention.
fn build_enterprise_config(args: &Args, tier: ProductTier) -> onto_enterprise::EnterpriseConfig {
    // Start with default config based on tier
    let mut config = if tier == ProductTier::EnterpriseGov {
        onto_enterprise::default_gov_config()
    } else {
        onto_enterprise::EnterpriseConfig::default()
    };
    
    // Override with command line arguments for Gov/Finance edition
    if tier == ProductTier::EnterpriseGov {
        // Configure encryption
        #[cfg(feature = "encryption")]
        {
            config.encryption.storage_encryption = args.encryption_enabled;
            if let Some(ref key_file) = args.master_key_file {
                config.encryption.master_key_source = onto_enterprise::encryption::KeySource::File(
                    key_file.to_string_lossy().to_string()
                );
            } else if let Some(ref env_var) = args.master_key_env {
                config.encryption.master_key_source = onto_enterprise::encryption::KeySource::Env(
                    env_var.clone()
                );
            }
        }
        
        // Configure audit retention
        #[cfg(feature = "audit-retention")]
        {
            config.audit_retention.enabled = args.audit || tier == ProductTier::EnterpriseGov;
            config.audit_retention.log_dir = args.audit_dir.clone();
            config.audit_retention.retention_days = args.audit_retention_days;
            config.audit_retention.compress_rotated = !args.audit_no_compress;
        }
    }
    
    // Load from config file if specified
    if let Some(ref config_path) = args.enterprise_config {
        match std::fs::read_to_string(config_path) {
            Ok(content) => {
                match serde_json::from_str::<onto_enterprise::EnterpriseConfig>(&content) {
                    Ok(file_config) => {
                        config = file_config;
                        tracing::info!("Loaded enterprise config from {:?}", config_path);
                    }
                    Err(e) => {
                        tracing::error!(
                            "Failed to parse enterprise config file {:?}: {}. Using defaults.",
                            config_path, e
                        );
                    }
                }
            }
            Err(e) => {
                tracing::error!(
                    "Failed to read enterprise config file {:?}: {}. Using defaults.",
                    config_path, e
                );
            }
        }
    }
    
    config
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
    audit_config: audit::AuditConfig,
    raft_node_id: Option<u64>,
    api_keys_file: Option<PathBuf>,
    cors_origins: String,
    tls_config: Option<tls::TlsConfig>,
    graph: Arc<onto_graph::GraphStore>,
) -> Result<()> {
    let audit = Arc::new(audit::AuditLogger::new(audit_config));
    let state = http::AppState { executor, metrics, graph, audit, raft_node_id };
    let auth_state = AuthState::new(&auth_config).with_metrics(state.metrics.clone());

    // Enable hot-reload for auth config file (check every 10 seconds)
    if let Some(ref api_keys_path) = api_keys_file {
        let auth_state_for_reload = auth_state.clone().with_config_path(api_keys_path.clone());
        auth_state_for_reload.start_reload_watcher(10);
        eprintln!("Auth config hot-reload enabled (checking every 10s): {:?}", api_keys_path);
    }

    let rate_limiter = RateLimiter::new(rate_limit_config.clone()).with_metrics(state.metrics.clone());

    // Spawn background task to clean up stale rate limit buckets every 5 minutes
    let limiter_cleanup = rate_limiter.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(300));
        loop {
            interval.tick().await;
            limiter_cleanup.cleanup_stale(std::time::Duration::from_secs(600)).await;
        }
    });

    // Shared config store for cross-node config sync (Raft integration)
    let config_store = onto_raft::SharedConfigStore::new(api_keys_file.clone());
    config_store.load_from_disk();

    // Register callback: when config changes, reload AuthState in-memory
    let auth_for_callback = auth_state.clone();
    config_store.on_change(Box::new(move |json_bytes: &[u8]| {
        // Parse the new config
        if let Ok(config) = serde_json::from_slice::<auth::AuthConfig>(json_bytes) {
            // Update AuthState keys in-memory
            let mut keys = std::collections::HashMap::new();
            for k in &config.keys {
                keys.insert(k.key.clone(), (k.description.clone(), k.permission.clone(), k.rate_limit, k.allowed_ips.clone()));
            }
            // AuthState uses parking_lot::RwLock, so we can update it directly
            *auth_for_callback.keys.write() = keys;
            tracing::info!("AuthState updated from config change ({} keys)", config.keys.len());
        }
    }));

    // Admin API (requires Admin permission via existing auth middleware)
    let admin_state = admin::AdminState {
        auth: auth_state.clone(),
        config_path: api_keys_file.clone(),
        metrics: state.metrics.clone(),
        config_store: Some(config_store.clone()),
        cluster_manager: None,
        audit: Some(state.audit.clone()),
    };

    let app = http::build_router_with_auth(state, auth_state.clone(), rate_limiter, admin_state, &cors_origins);

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

    if let Some(tls) = tls_config {
        // TLS-enabled HTTPS server
        let server_config = tls.build_server_config()
            .map_err(|e| onto_core::CoreError::Custom(format!("TLS config error: {}", e)))?;
        let addr: std::net::SocketAddr = addr.parse()
            .map_err(|e| onto_core::CoreError::Custom(format!("Invalid address '{}': {}", addr, e)))?;
        let rustls_config = axum_server::tls_rustls::RustlsConfig::from_config(Arc::new(server_config));
        axum_server::bind_rustls(addr, rustls_config)
            .serve(app.into_make_service_with_connect_info::<std::net::SocketAddr>())
            .await
            .map_err(|e| onto_core::CoreError::Custom(format!("HTTPS server error: {}", e)))?;
    } else {
        // Plain HTTP server (no TLS)
        let listener = tokio::net::TcpListener::bind(addr).await
            .map_err(|e| onto_core::CoreError::Io(e))?;
        axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>()).await
            .map_err(|e| onto_core::CoreError::Custom(format!("HTTP server error: {}", e)))?;
    }
    Ok(())
}

/// Runs a Raft consensus node for distributed replication.
async fn run_raft_node(
    node_id: u64,
    listen_addr: &str,
    peers_str: &str,
) -> Result<()> {
    use onto_raft::RaftNodeManager;

    // Parse peer list: "2=127.0.0.1:9001,3=127.0.0.1:9002"
    let mut initial_members = std::collections::BTreeMap::new();
    if !peers_str.is_empty() {
        for pair in peers_str.split(',') {
            let parts: Vec<&str> = pair.trim().split('=').collect();
            if parts.len() == 2 {
                let id: u64 = parts[0].trim().parse()
                    .map_err(|_| onto_core::CoreError::Custom(format!("invalid peer ID: {}", parts[0])))?;
                initial_members.insert(id, parts[1].trim().to_string());
            }
        }
    }

    let config = onto_raft::manager::RaftNodeConfig {
        node_id,
        listen_addr: listen_addr.to_string(),
        initial_members,
    };

    let manager = RaftNodeManager::new(config);
    println!("Raft cluster config:\n{}", manager.export_config());

    // Start Raft TCP server
    let server = onto_raft::network::RaftTcpServer::new(listen_addr);
    server.start().await.map_err(|e| onto_core::CoreError::Custom(e.to_string()))?;

    Ok(())
}

/// Maximum concurrent TCP connections.
const MAX_TCP_CONNECTIONS: usize = 256;

/// Maximum line size for TCP protocol (1 MB).
const MAX_LINE_BYTES: usize = 1024 * 1024;

/// Runs the async TCP server, accepting client connections.
async fn run_tcp_server(addr: &str, executor: Arc<QueryExecutor>, metrics: Arc<metrics::Metrics>) -> Result<()> {
    let listener = TcpListener::bind(addr).await
        .map_err(|e| onto_core::CoreError::Io(e))?;
    let conn_semaphore = Arc::new(tokio::sync::Semaphore::new(MAX_TCP_CONNECTIONS));

    println!("Listening on {}", addr);
    println!("Connect with: ontodb-cli {}", addr);

    loop {
        let (stream, _peer_addr) = listener.accept().await
            .map_err(|e| onto_core::CoreError::Io(e))?;

        let permit = match conn_semaphore.clone().acquire_owned().await {
            Ok(p) => p,
            Err(_) => break,
        };

        let executor = Arc::clone(&executor);
        let metrics = Arc::clone(&metrics);
        metrics.tcp_connections_total.inc();
        metrics.tcp_connections_active.inc();

        tokio::spawn(async move {
            if let Err(e) = handle_client(stream, &executor, &metrics).await {
                eprintln!("Client error: {}", e);
            }
            metrics.tcp_connections_active.dec();
            drop(permit);
        });
    }
    Ok(())
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

        if line.len() > MAX_LINE_BYTES {
            writer.write_all(b"ERR: line too long (max 1MB)\n\0").await?;
            line.clear();
            continue;
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

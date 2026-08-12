//! PostgreSQL wire protocol (v3) connector for OntoDB.
//!
//! Allows tools like `psql`, DBeaver, pgAdmin, and any PostgreSQL client
//! to connect to OntoDB directly.
//!
//! Supports the Simple Query protocol:
//! - Startup handshake with authentication
//! - SQL query execution
//! - Result streaming (RowDescription + DataRow + CommandComplete)
//! - Error reporting

use std::sync::Arc;

use bytes::{Buf, BufMut, BytesMut};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use onto_query::{QueryAst, QueryExecutor, QueryParser};
use crate::auth::{AuthConfig, AuthState};

/// PostgreSQL protocol version: 3.0
const PROTOCOL_VERSION: u32 = 196608; // 3 << 16 | 0

/// Message type constants (server → client)
const AUTH_OK: u8 = b'R';
const BACKEND_KEY: u8 = b'K';
const READY_FOR_QUERY: u8 = b'Z';
const ROW_DESCRIPTION: u8 = b'T';
const DATA_ROW: u8 = b'D';
const COMMAND_COMPLETE: u8 = b'C';
const ERROR_RESPONSE: u8 = b'E';
const PARAMETER_STATUS: u8 = b'S';

/// Message type constants (client → server)
const QUERY_MSG: u8 = b'Q';
const TERMINATE_MSG: u8 = b'X';

/// Transaction status indicators
const TXN_IDLE: u8 = b'I';

/// Maximum concurrent PG Wire connections.
const MAX_PGWIRE_CONNECTIONS: usize = 128;

/// Maximum message size for PG Wire protocol (16 MB).
const MAX_PGWIRE_MESSAGE_SIZE: usize = 16 * 1024 * 1024;

/// Run the PG wire protocol server.
pub async fn run_pgwire_server(
    addr: &str,
    executor: Arc<QueryExecutor>,
    metrics: Arc<crate::metrics::Metrics>,
    auth_config: AuthConfig,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let auth = AuthState::new(&auth_config);
    let conn_semaphore = Arc::new(tokio::sync::Semaphore::new(MAX_PGWIRE_CONNECTIONS));
    println!("PG wire protocol listening on {}", addr);
    println!("Connect with: psql -h 127.0.0.1 -p {} -d ontodb", addr.split(':').last().unwrap_or("5432"));

    loop {
        let (stream, peer) = listener.accept().await?;

        // Enforce connection limit
        let permit = match conn_semaphore.clone().acquire_owned().await {
            Ok(p) => p,
            Err(_) => break Ok(()),
        };

        let executor = Arc::clone(&executor);
        let metrics = Arc::clone(&metrics);
        let auth = auth.clone();
        metrics.tcp_connections_total.inc();
        metrics.tcp_connections_active.inc();

        tokio::spawn(async move {
            if let Err(e) = handle_pgwire_client(stream, &executor, &auth).await {
                eprintln!("PG wire client error ({}): {}", peer, e);
            }
            metrics.tcp_connections_active.dec();
            drop(permit);
        });
    }
}

/// Handle a single PG wire protocol client connection.
async fn handle_pgwire_client(
    mut stream: TcpStream,
    executor: &QueryExecutor,
    auth: &AuthState,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut buf = BytesMut::with_capacity(8192);

    // Step 1: Read startup message
    loop {
        let n = stream.read_buf(&mut buf).await?;
        if n == 0 {
            return Ok(());
        }

        // Startup message: length(4) + version(4) + params...
        if buf.len() < 8 {
            continue;
        }

        let len = (&buf[0..4]).get_u32() as usize;
        if buf.len() < len {
            continue;
        }

        let version = (&buf[4..8]).get_u32();
        if version == 80877103 {
            // SSLRequest — reject SSL (for now)
            stream.write_all(b"N").await?;
            buf.advance(len);
            continue;
        }

        if version == 80877102 {
            // CancelRequest — ignore for now
            buf.advance(len);
            return Ok(());
        }

        if version != PROTOCOL_VERSION {
            return Err(format!("Unsupported protocol version: {}", version).into());
        }

        // Parse startup parameters (null-terminated key-value pairs)
        let params_data = &buf[8..len];
        let params = parse_startup_params(params_data);
        buf.advance(len);

        // Step 2: Authenticate (MD5 password)
        if auth.enabled {
            // Extract username from startup params
            let username = params.iter()
                .find(|(k, _)| k == "user")
                .map(|(_, v)| v.as_str())
                .unwrap_or("ontodb");

            // Generate random 4-byte salt for MD5 challenge
            let mut salt = [0u8; 4];
            {
                use std::io::Write;
                // Use time + PID + counter for non-crypto salt (just needs to be unique)
                let t = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos();
                let pid = std::process::id();
                static SALT_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                let ctr = SALT_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let seed = format!("{}{}{}", t, pid, ctr);
                let hash = md5::compute(seed.as_bytes());
                salt.copy_from_slice(&hash[..4]);
            }

            // Send AuthenticationMD5Password (type 5) with salt
            let mut auth_req = BytesMut::new();
            auth_req.put_u8(AUTH_OK);
            auth_req.put_u32(12); // length = 4 + 4 + 4
            auth_req.put_i32(5); // AuthenticationMD5Password
            auth_req.extend_from_slice(&salt);
            stream.write_all(&auth_req).await?;

            // Read password message (client sends "md5" + 32 hex chars)
            let password = loop {
                let n = stream.read_buf(&mut buf).await?;
                if n == 0 {
                    return Ok(());
                }
                if buf.len() < 5 {
                    continue;
                }
                let msg_type = buf[0];
                let msg_len = (&buf[1..5]).get_u32() as usize;
                if buf.len() < msg_len + 1 {
                    continue;
                }
                if msg_type == b'p' {
                    // PasswordMessage: the password is null-terminated
                    let pw_bytes = buf[5..msg_len + 1].to_vec();
                    let pw = std::str::from_utf8(&pw_bytes)
                        .unwrap_or("")
                        .trim_end_matches('\0');
                    buf.advance(msg_len + 1);
                    break pw.to_string();
                } else {
                    buf.advance(msg_len + 1);
                }
            };

            // Validate MD5 hash against stored API keys
            if auth.validate_md5(&password, username, &salt).is_none() {
                let mut err_msg = BytesMut::new();
                err_msg.put_u8(ERROR_RESPONSE);
                // Build error fields
                let err_body = b"SFATAL\0C28P01\0Mauthentication failed\0\0";
                err_msg.put_u32(4 + err_body.len() as u32);
                err_msg.extend_from_slice(err_body);
                stream.write_all(&err_msg).await?;
                return Ok(());
            }
        }
        // Send AuthenticationOk
        let mut auth_msg = BytesMut::new();
        auth_msg.put_u8(AUTH_OK);
        auth_msg.put_u32(8); // length
        auth_msg.put_i32(0); // auth type = OK
        stream.write_all(&auth_msg).await?;
        break;
    }

    // Step 3: Send BackendKeyData (process_id, secret_key)
    let mut key_msg = BytesMut::new();
    key_msg.put_u8(BACKEND_KEY);
    key_msg.put_u32(12); // length
    key_msg.put_i32(std::process::id() as i32);
    // Generate a cryptographically random per-connection secret for cancel request validation
    use rand::Rng;
    let secret: i32 = rand::thread_rng().gen();
    key_msg.put_i32(secret);
    stream.write_all(&key_msg).await?;

    // Step 4: Send ParameterStatus messages
    send_parameter_status(&mut stream, "server_version", "0.1.0").await?;
    send_parameter_status(&mut stream, "server_encoding", "UTF8").await?;
    send_parameter_status(&mut stream, "client_encoding", "UTF8").await?;
    send_parameter_status(&mut stream, "DateStyle", "ISO, MDY").await?;
    send_parameter_status(&mut stream, "integer_datetimes", "on").await?;

    // Step 5: Send ReadyForQuery
    send_ready_for_query(&mut stream, TXN_IDLE).await?;

    // Step 6: Query loop
    loop {
        buf.clear();
        let n = stream.read_buf(&mut buf).await?;
        if n == 0 {
            break;
        }

        // Messages: type(1) + length(4) + payload
        while buf.len() >= 5 {
            let msg_type = buf[0];
            let msg_len = (&buf[1..5]).get_u32() as usize;

            // Enforce message size limit
            if msg_len > MAX_PGWIRE_MESSAGE_SIZE {
                return Err(format!("PG wire message too large: {} bytes (max {})", msg_len, MAX_PGWIRE_MESSAGE_SIZE).into());
            }

            if buf.len() < msg_len + 1 {
                break; // incomplete message
            }

            let payload = buf[5..msg_len + 1].to_vec();
            buf.advance(msg_len + 1);

            match msg_type {
                QUERY_MSG => {
                    let query = std::str::from_utf8(&payload)
                        .unwrap_or("")
                        .trim_end_matches('\0')
                        .trim_end_matches(';')
                        .trim();

                    if query.is_empty() {
                        send_ready_for_query(&mut stream, TXN_IDLE).await?;
                        continue;
                    }

                    handle_query(&mut stream, executor, query).await?;
                }
                TERMINATE_MSG => {
                    return Ok(());
                }
                _ => {
                    // Unknown message type — skip
                    eprintln!("Unknown PG wire message type: {}", msg_type);
                }
            }
        }
    }

    Ok(())
}

/// Execute a query and send results in PG wire protocol format.
async fn handle_query(
    stream: &mut TcpStream,
    executor: &QueryExecutor,
    query: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    match QueryParser::parse(query) {
        Ok(ast) => {
            let result = if QueryExecutor::is_read_only_query(&ast) {
                executor.execute_read(&ast)
            } else {
                executor.execute(&ast)
            };

            match result {
                Ok(query_result) => {
                    match query_result {
                        onto_query::QueryResult::Rows(rows) => {
                            if rows.is_empty() {
                                // No rows — send empty CommandComplete
                                let tag = determine_tag(&ast, 0);
                                send_command_complete(stream, &tag).await?;
                            } else {
                                // RowDescription
                                send_row_description(stream, &rows[0]).await?;

                                // DataRows
                                for row in &rows {
                                    send_data_row(stream, row).await?;
                                }

                                // CommandComplete
                                let tag = determine_tag(&ast, rows.len());
                                send_command_complete(stream, &tag).await?;
                            }
                        }
                        onto_query::QueryResult::Success(msg) => {
                            // DDL or DML success
                            send_command_complete(stream, &msg).await?;
                        }
                    }
                }
                Err(e) => {
                    send_error(stream, "ERROR", &format!("{}", e)).await?;
                }
            }
        }
        Err(e) => {
            send_error(stream, "SYNTAX_ERROR", &format!("Parse error: {}", e)).await?;
        }
    }

    send_ready_for_query(stream, TXN_IDLE).await?;
    Ok(())
}

/// Send a RowDescription message.
async fn send_row_description(
    stream: &mut TcpStream,
    sample_row: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fields: Vec<&String> = sample_row.keys().collect();
    let num_fields = fields.len() as i16;

    let mut msg = BytesMut::new();
    msg.put_u8(ROW_DESCRIPTION);

    let mut payload = BytesMut::new();
    payload.put_i16(num_fields);

    for field_name in &fields {
        payload.extend_from_slice(field_name.as_bytes());
        payload.put_u8(0);
        payload.put_i32(0);
        payload.put_i16(0);
        payload.put_i32(guess_type_oid(sample_row.get(*field_name)));
        payload.put_i16(-1);
        payload.put_i32(-1);
        payload.put_i16(0);
    }

    msg.put_u32(payload.len() as u32 + 4);
    msg.extend_from_slice(&payload);
    stream.write_all(&msg).await?;
    Ok(())
}

/// Send a DataRow message.
async fn send_data_row(
    stream: &mut TcpStream,
    row: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut msg = BytesMut::new();
    msg.put_u8(DATA_ROW);

    let mut payload = BytesMut::new();
    let num_cols = row.len() as i16;
    payload.put_i16(num_cols);

    for value in row.values() {
        let text = value_to_text(value);
        match text {
            None => payload.put_i32(-1),
            Some(s) => {
                let bytes = s.as_bytes();
                payload.put_i32(bytes.len() as i32);
                payload.extend_from_slice(bytes);
            }
        }
    }

    msg.put_u32(payload.len() as u32 + 4);
    msg.extend_from_slice(&payload);
    stream.write_all(&msg).await?;
    Ok(())
}

/// Send a CommandComplete message.
async fn send_command_complete(
    stream: &mut TcpStream,
    tag: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut msg = BytesMut::new();
    msg.put_u8(COMMAND_COMPLETE);
    let tag_bytes = tag.as_bytes();
    msg.put_u32(tag_bytes.len() as u32 + 5); // length includes null terminator
    msg.extend_from_slice(tag_bytes);
    msg.put_u8(0); // null terminator
    stream.write_all(&msg).await?;
    Ok(())
}

/// Send an ErrorResponse message.
async fn send_error(
    stream: &mut TcpStream,
    code: &str,
    message: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut msg = BytesMut::new();
    msg.put_u8(ERROR_RESPONSE);

    let mut payload = BytesMut::new();
    // Severity field
    payload.put_u8(b'S');
    payload.extend_from_slice(b"ERROR\0");
    // Code field
    payload.put_u8(b'C');
    payload.extend_from_slice(code.as_bytes());
    payload.put_u8(0);
    // Message field
    payload.put_u8(b'M');
    payload.extend_from_slice(message.as_bytes());
    payload.put_u8(0);
    // Terminator
    payload.put_u8(0);

    msg.put_u32(payload.len() as u32 + 4);
    msg.extend_from_slice(&payload);
    stream.write_all(&msg).await?;
    Ok(())
}

/// Send a ReadyForQuery message.
async fn send_ready_for_query(
    stream: &mut TcpStream,
    status: u8,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut msg = BytesMut::new();
    msg.put_u8(READY_FOR_QUERY);
    msg.put_u32(5); // length
    msg.put_u8(status);
    stream.write_all(&msg).await?;
    Ok(())
}

/// Send a ParameterStatus message.
async fn send_parameter_status(
    stream: &mut TcpStream,
    name: &str,
    value: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut msg = BytesMut::new();
    msg.put_u8(PARAMETER_STATUS);

    let mut payload = BytesMut::new();
    payload.extend_from_slice(name.as_bytes());
    payload.put_u8(0);
    payload.extend_from_slice(value.as_bytes());
    payload.put_u8(0);

    msg.put_u32(payload.len() as u32 + 4);
    msg.extend_from_slice(&payload);
    stream.write_all(&msg).await?;
    Ok(())
}

/// Parse startup message parameters (null-terminated key-value pairs).
fn parse_startup_params(data: &[u8]) -> Vec<(String, String)> {
    let mut params = Vec::new();
    let mut i = 0;
    while i < data.len() {
        if data[i] == 0 {
            break; // terminator
        }
        let key = read_null_string(data, &mut i);
        let value = read_null_string(data, &mut i);
        params.push((key, value));
    }
    params
}

fn read_null_string(data: &[u8], pos: &mut usize) -> String {
    let start = *pos;
    while *pos < data.len() && data[*pos] != 0 {
        *pos += 1;
    }
    let s = String::from_utf8_lossy(&data[start..*pos]).to_string();
    if *pos < data.len() {
        *pos += 1; // skip null terminator
    }
    s
}

/// Convert a serde_json::Value to text representation for PG wire.
fn value_to_text(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::Null => None,
        serde_json::Value::Bool(b) => Some(if *b { "t".to_string() } else { "f".to_string() }),
        serde_json::Value::Number(n) => Some(n.to_string()),
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Array(arr) => {
            let items: Vec<String> = arr.iter().map(|v| {
                value_to_text(v).unwrap_or_else(|| "NULL".to_string())
            }).collect();
            Some(format!("{{{}}}", items.join(",")))
        }
        serde_json::Value::Object(map) => {
            let items: Vec<String> = map.iter().map(|(k, v)| {
                let val = value_to_text(v).unwrap_or_else(|| "NULL".to_string());
                format!("\"{}\"=>\"{}\"", k, val)
            }).collect();
            Some(format!("{{{}}}", items.join(",")))
        }
    }
}

/// Guess the PG type OID from a serde_json::Value.
fn guess_type_oid(value: Option<&serde_json::Value>) -> i32 {
    match value {
        Some(serde_json::Value::Number(n)) => {
            if n.is_i64() || n.is_u64() { 23 } else { 700 }
        }
        Some(serde_json::Value::Bool(_)) => 16,
        Some(serde_json::Value::String(_)) => 25,
        _ => 25,
    }
}

/// Determine the CommandComplete tag from query AST and row count.
fn determine_tag(ast: &QueryAst, row_count: usize) -> String {
    match ast {
        QueryAst::Select { .. } => format!("SELECT {}", row_count),
        QueryAst::Insert { .. } => format!("INSERT 0 {}", row_count),
        QueryAst::Update { .. } => format!("UPDATE {}", row_count),
        QueryAst::Delete { .. } => format!("DELETE {}", row_count),
        _ => "OK".to_string(),
    }
}

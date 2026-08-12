//! MySQL wire protocol (v4.1) connector for OntoDB.
//!
//! Allows tools like `mysql` CLI, DBeaver, Navicat, MySQL Workbench,
//! and any MySQL client to connect to OntoDB directly.
//!
//! Supports:
//! - Handshake with authentication (mysql_native_password)
//! - COM_QUERY: SQL query execution
//! - COM_PING: keepalive
//! - COM_QUIT: graceful disconnect
//! - Text protocol result sets (columns + rows)
//!
//! MySQL Protocol Reference:
//! - Packet: [3-byte length][1-byte seq][payload]
//! - Handshake: server sends greeting, client responds with auth
//! - Commands: 1-byte command type + payload
//! - ResultSet: column_count, column_def, eof, row, eof

use std::sync::Arc;

use bytes::{Buf, BufMut, BytesMut};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use onto_query::{QueryExecutor, QueryParser};
use crate::auth::{AuthConfig, AuthState};

// ── MySQL Protocol Constants ──

/// MySQL protocol version string.
const MYSQL_VERSION: &str = "8.0.35-ontodb\0";

/// Server capability flags (CLIENT_PROTOCOL_41 | CLIENT_SECURE_CONNECTION | etc.)
const SERVER_CAPABILITIES: u32 =
    0x00000200  // CLIENT_PROTOCOL_41
    | 0x00008000  // CLIENT_SECURE_CONNECTION
    | 0x00000001  // CLIENT_LONG_PASSWORD
    | 0x00000002  // CLIENT_FOUND_ROWS
    | 0x00000004  // CLIENT_LONG_FLAG
    | 0x00000008  // CLIENT_CONNECT_WITH_DB
    | 0x00000200  // CLIENT_PROTOCOL_41
    | 0x00000400  // CLIENT_OPTIONAL_RESULTSET_METADATA
    | 0x00002000  // CLIENT_PLUGIN_AUTH
    | 0x00080000  // CLIENT_PLUGIN_AUTH_LENENC_CLIENT_DATA
    | 0x00200000; // CLIENT_CAN_HANDLE_EXPIRED_PASSWORDS

/// Server status flags.
const SERVER_STATUS_AUTOCOMMIT: u16 = 0x0002;

/// Character set: utf8mb4 (255 = 0xFF).
const CHARSET_UTF8MB4: u8 = 0x21;

/// Command types (client → server).
const COM_QUERY: u8 = 0x03;
const COM_PING: u8 = 0x0E;
const COM_QUIT: u8 = 0x01;
const COM_INIT_DB: u8 = 0x02;

/// Column type constants (MySQL text protocol).
#[allow(dead_code)]
const MYSQL_TYPE_VAR_STRING: u8 = 0xFD;
#[allow(dead_code)]
const MYSQL_TYPE_LONGLONG: u8 = 0x08;
#[allow(dead_code)]
const MYSQL_TYPE_DOUBLE: u8 = 0x05;
#[allow(dead_code)]
const MYSQL_TYPE_LONG: u8 = 0x03;
#[allow(dead_code)]
const MYSQL_TYPE_TINY: u8 = 0x01;
#[allow(dead_code)]
const MYSQL_TYPE_NULL: u8 = 0x06;

/// Maximum packet size (16 MB).
const MAX_PACKET_SIZE: u32 = 16 * 1024 * 1024;

/// Maximum concurrent MySQL connections.
const MAX_MYSQL_CONNECTIONS: usize = 128;

// ── Server Entry Point ──

/// Run the MySQL wire protocol server.
pub async fn run_mysql_server(
    addr: &str,
    executor: Arc<QueryExecutor>,
    metrics: Arc<crate::metrics::Metrics>,
    auth_config: AuthConfig,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let auth = AuthState::new(&auth_config);
    let conn_semaphore = Arc::new(tokio::sync::Semaphore::new(MAX_MYSQL_CONNECTIONS));
    println!("MySQL protocol listening on {}", addr);
    println!("Connect with: mysql -h 127.0.0.1 -P {} -u root", addr.split(':').next_back().unwrap_or("3306"));

    loop {
        let (stream, peer) = listener.accept().await?;

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
            if let Err(e) = handle_mysql_client(stream, &executor, &auth).await {
                eprintln!("MySQL client error ({}): {}", peer, e);
            }
            metrics.tcp_connections_active.dec();
            drop(permit);
        });
    }
}

// ── Client Handler ──

/// Handle a single MySQL client connection.
async fn handle_mysql_client(
    mut stream: TcpStream,
    executor: &QueryExecutor,
    auth: &AuthState,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Step 1: Send HandshakeInitializationPacket
    let handshake = build_handshake();
    write_packet(&mut stream, 0, &handshake).await?;

    // Step 2: Read HandshakeResponse
    let (seq, response) = read_packet(&mut stream).await?;
    if response.is_empty() {
        return Ok(());
    }

    // Parse response: skip 4-byte capability flags + 4-byte max_packet_size + 1-byte charset + 23 reserved
    // Then read null-terminated username, then auth data
    if response.len() < 32 {
        return Err("Malformed handshake response".into());
    }

    let username = read_null_terminated(&response[32..]);
    let provided_password = if response.len() > 32 + username.len() + 1 {
        let auth_data_start = 32 + username.len() + 1;
        if auth_data_start < response.len() {
            &response[auth_data_start..]
        } else {
            &[]
        }
    } else {
        &[]
    };

    // Authenticate
    if auth.enabled {
        let password_str = std::str::from_utf8(provided_password).unwrap_or("");
        if auth.validate(password_str).is_none() {
            send_error_packet(&mut stream, seq + 1, 1045, "28000", "Access denied for user").await?;
            return Ok(());
        }
    }

    // Step 3: Send OK packet (handshake success)
    send_ok_packet(&mut stream, seq + 1).await?;

    // Step 4: Command loop
    let mut buf = BytesMut::with_capacity(8192);
    loop {
        let n = stream.read_buf(&mut buf).await?;
        if n == 0 {
            break;
        }

        // Process complete packets
        while buf.len() >= 4 {
            let pkt_len = (buf[0] as u32) | ((buf[1] as u32) << 8) | ((buf[2] as u32) << 16);
            let _seq = buf[3];

            if pkt_len > MAX_PACKET_SIZE {
                return Err(format!("Packet too large: {} bytes", pkt_len).into());
            }

            if buf.len() < 4 + pkt_len as usize {
                break; // incomplete
            }

            let payload = buf[4..4 + pkt_len as usize].to_vec();
            buf.advance(4 + pkt_len as usize);

            if payload.is_empty() {
                continue;
            }

            let cmd = payload[0];
            let cmd_payload = &payload[1..];

            match cmd {
                COM_QUERY => {
                    let query = std::str::from_utf8(cmd_payload).unwrap_or("").trim();
                    if query.is_empty() {
                        send_ok_packet(&mut stream, 0).await?;
                        continue;
                    }
                    handle_mysql_query(&mut stream, executor, query).await?;
                }
                COM_PING => {
                    send_ok_packet(&mut stream, 1).await?;
                }
                COM_INIT_DB => {
                    // Accept but ignore database switch
                    send_ok_packet(&mut stream, 2).await?;
                }
                COM_QUIT => {
                    return Ok(());
                }
                _ => {
                    // Unknown command — send error
                    send_error_packet(&mut stream, 1, 1047, "08S01", &format!("Unknown command: 0x{:02X}", cmd)).await?;
                }
            }
        }
    }

    Ok(())
}

// ── Query Handler ──

/// Execute a query and send results in MySQL text protocol format.
async fn handle_mysql_query(
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
                                send_ok_packet(stream, 0).await?;
                            } else {
                                send_result_set(stream, &rows).await?;
                            }
                        }
                        onto_query::QueryResult::Success(msg) => {
                            send_ok_packet_with_message(stream, 0, &msg).await?;
                        }
                    }
                }
                Err(e) => {
                    send_error_packet(stream, 1, 1105, "HY000", &format!("{}", e)).await?;
                }
            }
        }
        Err(e) => {
            send_error_packet(stream, 1, 1064, "42000", &format!("Parse error: {}", e)).await?;
        }
    }
    Ok(())
}

// ── Result Set ──

/// Send a result set (column definitions + rows + EOF markers).
async fn send_result_set(
    stream: &mut TcpStream,
    rows: &[serde_json::Map<String, serde_json::Value>],
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if rows.is_empty() {
        send_ok_packet(stream, 0).await?;
        return Ok(());
    }

    let columns: Vec<String> = rows[0].keys().cloned().collect();
    let col_count = columns.len();

    // 1. Column count (length-encoded integer)
    let mut pkt = BytesMut::new();
    write_length_encoded_int(&mut pkt, col_count as u64);
    write_packet(stream, 1, &pkt).await?;

    // 2. Column definitions
    for (i, col_name) in columns.iter().enumerate() {
        let mut seq = (i as u8) + 2;
        if seq > 250 { seq %= 250; }
        let col_def = build_column_definition(col_name, MYSQL_TYPE_VAR_STRING);
        write_packet(stream, seq, &col_def).await?;
    }

    // 3. EOF after columns
    let eof_seq = (col_count as u8) + 2;
    write_packet(stream, eof_seq, &[0xFE]).await?;

    // 4. Rows
    for (row_idx, row) in rows.iter().enumerate() {
        let mut row_data = BytesMut::new();
        for col_name in &columns {
            match row.get(col_name) {
                Some(serde_json::Value::String(s)) => {
                    write_length_encoded_bytes(&mut row_data, s.as_bytes());
                }
                Some(serde_json::Value::Number(n)) => {
                    write_length_encoded_bytes(&mut row_data, n.to_string().as_bytes());
                }
                Some(serde_json::Value::Bool(b)) => {
                    write_length_encoded_bytes(&mut row_data, if *b { b"1" } else { b"0" });
                }
                Some(serde_json::Value::Null) | None => {
                    row_data.put_u8(0xFB); // NULL
                }
                Some(other) => {
                    write_length_encoded_bytes(&mut row_data, other.to_string().as_bytes());
                }
            }
        }
        let row_seq = (row_idx as u8) + eof_seq + 1;
        write_packet(stream, row_seq, &row_data).await?;
    }

    // 5. EOF after rows
    let final_seq = (rows.len() as u8) + eof_seq + 1;
    write_packet(stream, final_seq, &[0xFE]).await?;

    Ok(())
}

// ── Packet Builders ──

/// Build MySQL handshake packet.
fn build_handshake() -> Vec<u8> {
    let mut pkt = Vec::new();

    // Protocol version
    pkt.extend_from_slice(b"\x0a");

    // Server version (null-terminated)
    pkt.extend_from_slice(MYSQL_VERSION.as_bytes());

    // Connection ID (4 bytes)
    pkt.extend_from_slice(&std::process::id().to_le_bytes()[..4]);

    // Auth-plugin-data-part-1 (8 bytes) — pseudo-random challenge
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let pid = std::process::id() as u64;
    let seed = now ^ ((pid as u128) << 64);
    let challenge: [u8; 8] = (seed as u64).to_le_bytes();
    pkt.extend_from_slice(&challenge);

    // Filler (1 byte)
    pkt.push(0x00);

    // Capability flags lower 2 bytes
    pkt.extend_from_slice(&(SERVER_CAPABILITIES as u16).to_le_bytes());

    // Character set
    pkt.push(CHARSET_UTF8MB4);

    // Status flags
    pkt.extend_from_slice(&SERVER_STATUS_AUTOCOMMIT.to_le_bytes());

    // Capability flags upper 2 bytes
    pkt.extend_from_slice(&((SERVER_CAPABILITIES >> 16) as u16).to_le_bytes());

    // Auth plugin data length
    pkt.push(21); // 8 + 13

    // Reserved (10 bytes)
    pkt.extend_from_slice(&[0u8; 10]);

    // Auth-plugin-data-part-2 (13 bytes) — pseudo-random
    let challenge2_bytes = ((seed >> 8) as u64).to_le_bytes();
    let mut challenge2 = [0u8; 13];
    challenge2[..8].copy_from_slice(&challenge2_bytes);
    challenge2[8..].copy_from_slice(&pid.to_le_bytes()[..5]);
    pkt.extend_from_slice(&challenge2);
    pkt.push(0x00); // null terminator

    // Auth plugin name
    pkt.extend_from_slice(b"mysql_native_password\0");

    pkt
}

/// Build a column definition packet.
fn build_column_definition(name: &str, col_type: u8) -> BytesMut {
    let mut pkt = BytesMut::new();

    // Catalog (always "def")
    write_length_encoded_bytes(&mut pkt, b"def");

    // Schema (empty)
    write_length_encoded_bytes(&mut pkt, b"");

    // Table (empty)
    write_length_encoded_bytes(&mut pkt, b"");

    // Org table (empty)
    write_length_encoded_bytes(&mut pkt, b"");

    // Name
    write_length_encoded_bytes(&mut pkt, name.as_bytes());

    // Org name (empty)
    write_length_encoded_bytes(&mut pkt, b"");

    // Length of fixed-length fields (0x0c)
    pkt.extend_from_slice(&[0x0c, 0x3f, 0x00]); // length + charset (utf8mb4)

    // Column length (4 bytes)
    pkt.extend_from_slice(&65535u32.to_le_bytes());

    // Column type
    pkt.put_u8(col_type);

    // Flags (2 bytes)
    pkt.extend_from_slice(&0u16.to_le_bytes());

    // Decimals (1 byte)
    pkt.put_u8(0x00);

    // Filler (2 bytes)
    pkt.extend_from_slice(&[0x00, 0x00]);

    pkt
}

// ── Packet I/O ──

/// Write a MySQL packet: [3-byte length][1-byte seq][payload].
async fn write_packet(
    stream: &mut TcpStream,
    seq: u8,
    payload: &[u8],
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let len = payload.len() as u32;
    let mut header = BytesMut::with_capacity(4);
    header.put_u8((len & 0xFF) as u8);
    header.put_u8(((len >> 8) & 0xFF) as u8);
    header.put_u8(((len >> 16) & 0xFF) as u8);
    header.put_u8(seq);

    stream.write_all(&header).await?;
    stream.write_all(payload).await?;
    stream.flush().await?;
    Ok(())
}

/// Read a MySQL packet. Returns (sequence, payload).
async fn read_packet(
    stream: &mut TcpStream,
) -> Result<(u8, Vec<u8>), Box<dyn std::error::Error + Send + Sync>> {
    let mut header = [0u8; 4];
    stream.read_exact(&mut header).await?;

    let len = (header[0] as u32) | ((header[1] as u32) << 8) | ((header[2] as u32) << 16);
    let seq = header[3];

    if len > MAX_PACKET_SIZE {
        return Err(format!("Packet too large: {} bytes", len).into());
    }

    let mut payload = vec![0u8; len as usize];
    if len > 0 {
        stream.read_exact(&mut payload).await?;
    }

    Ok((seq, payload))
}

/// Send an OK packet.
async fn send_ok_packet(
    stream: &mut TcpStream,
    seq: u8,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    send_ok_packet_with_message(stream, seq, "").await
}

/// Send an OK packet with a message.
async fn send_ok_packet_with_message(
    stream: &mut TcpStream,
    seq: u8,
    message: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut pkt = BytesMut::new();
    pkt.put_u8(0x00); // OK marker
    write_length_encoded_int(&mut pkt, 0); // affected_rows
    write_length_encoded_int(&mut pkt, 0); // last_insert_id
    pkt.extend_from_slice(&SERVER_STATUS_AUTOCOMMIT.to_le_bytes()); // status flags
    pkt.extend_from_slice(&0u16.to_le_bytes()); // warnings

    if !message.is_empty() {
        write_length_encoded_bytes(&mut pkt, message.as_bytes());
    }

    write_packet(stream, seq, &pkt).await?;
    Ok(())
}

/// Send an error packet.
async fn send_error_packet(
    stream: &mut TcpStream,
    seq: u8,
    error_code: u16,
    sql_state: &str,
    message: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut pkt = BytesMut::new();
    pkt.put_u8(0xFF); // Error marker
    pkt.extend_from_slice(&error_code.to_le_bytes());
    pkt.put_u8(b'#'); // sql_state_marker
    pkt.extend_from_slice(sql_state.as_bytes());
    pkt.extend_from_slice(message.as_bytes());

    write_packet(stream, seq, &pkt).await?;
    Ok(())
}

// ── Helpers ──

/// Read a null-terminated string from bytes.
fn read_null_terminated(data: &[u8]) -> Vec<u8> {
    data.iter().take_while(|&&b| b != 0).copied().collect()
}

/// Write a length-encoded integer.
fn write_length_encoded_int(buf: &mut BytesMut, n: u64) {
    if n < 251 {
        buf.put_u8(n as u8);
    } else if n < 65536 {
        buf.put_u8(0xFC);
        buf.extend_from_slice(&(n as u16).to_le_bytes());
    } else if n < 16777216 {
        buf.put_u8(0xFD);
        let b = n.to_le_bytes();
        buf.extend_from_slice(&b[..3]);
    } else {
        buf.put_u8(0xFE);
        buf.extend_from_slice(&n.to_le_bytes());
    }
}

/// Write length-encoded bytes (length prefix + data).
fn write_length_encoded_bytes(buf: &mut BytesMut, data: &[u8]) {
    write_length_encoded_int(buf, data.len() as u64);
    buf.extend_from_slice(data);
}

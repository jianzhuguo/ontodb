#![allow(clippy::too_many_arguments)]
#![allow(clippy::type_complexity)]
#![allow(clippy::manual_strip)]
#![allow(clippy::needless_range_loop)]
#![allow(clippy::new_without_default)]
#![allow(clippy::collapsible_match)]
#![allow(clippy::if_same_then_else)]
#![allow(clippy::manual_checked_ops)]
#![allow(clippy::ptr_arg)]
#![allow(clippy::non_canonical_partial_ord_impl)]
#![allow(clippy::should_implement_trait)]
#![allow(clippy::sliced_string_as_bytes)]
#![allow(clippy::len_without_is_empty)]
#![allow(clippy::lines_filter_map_ok)]
#![allow(clippy::vec_init_then_push)]
#![allow(clippy::unnecessary_find_map)]
#![allow(clippy::unnecessary_unwrap)]
#![allow(clippy::result_large_err)]
#![allow(clippy::doc_lazy_continuation)]
// Copyright (c) 2024-2026 OntoDB Team
// Licensed under the Business Source License 1.1 (BUSL-1.1).
// See LICENSE for details. Change Date: 2031-09-15.
// On the Change Date, this file will be licensed under Apache License 2.0.
//! OntoDB CLI - Interactive command-line client for OntoDB server.
//!
//! Features:
//! - Interactive REPL with multi-line input (end statements with `;`)
//! - Special commands: \help, \d, \q, \c
//! - Query timing
//! - Aligned column output with borders
//! - Single-query mode (-q) and file mode (-f)
//! - Logical backup: dump / restore

use clap::{Parser, Subcommand};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::time::Instant;

#[derive(Parser, Debug)]
#[command(
    name = "ontodb-cli",
    about = "OntoDB interactive command-line client",
    long_about = "Connect to an OntoDB server and execute queries interactively.\n\n\
        Examples:\n  \
          ontodb-cli                              # connect to localhost:7913\n  \
          ontodb-cli 192.168.1.100:7913           # connect to remote server\n  \
          ontodb-cli -q \"SELECT * FROM Product\"   # single query, then exit\n  \
          ontodb-cli -f init.sql                  # execute SQL file\n  \
          ontodb-cli dump -o backup.jsonl         # dump all data to file\n  \
          ontodb-cli restore -i backup.jsonl      # restore from file"
)]
struct Args {
    /// Server address (host:port) — used by default (REPL/query) and subcommands
    #[arg(global = true, short = 'a', long, default_value = "127.0.0.1:7913")]
    address: String,

    /// Execute a single query and exit
    #[arg(short, long)]
    query: Option<String>,

    /// Execute queries from a SQL file
    #[arg(short, long)]
    file: Option<String>,

    /// TLS certificate file path for secure connections
    #[arg(long)]
    tls_cert: Option<String>,

    /// API key for authentication
    #[arg(long)]
    api_key: Option<String>,

    /// Subcommand (dump / restore)
    #[command(subcommand)]
    command: Option<CliCommand>,
}

#[derive(Subcommand, Debug)]
enum CliCommand {
    /// Dump all data (or a single class) to a JSON Lines file
    Dump {
        /// Output file path (default: stdout)
        #[arg(short, long)]
        output: Option<String>,

        /// Only dump a specific class (table)
        #[arg(short, long)]
        class: Option<String>,

        /// Output format: jsonl (default) or csv
        #[arg(short, long, default_value = "jsonl")]
        format: String,
    },

    /// Restore data from a JSON Lines file
    Restore {
        /// Input file path
        #[arg(short, long)]
        input: String,

        /// Only restore a specific class (table)
        #[arg(short, long)]
        class: Option<String>,

        /// Skip errors and continue (default: stop on first error)
        #[arg(long)]
        skip_errors: bool,
    },
}

fn main() {
    let args = Args::parse();

    match args.command {
        Some(CliCommand::Dump {
            output,
            class,
            format,
        }) => {
            run_dump(
                &args.address,
                output.as_deref(),
                class.as_deref(),
                &format,
                args.tls_cert.as_deref(),
                args.api_key.as_deref(),
            );
        }
        Some(CliCommand::Restore {
            input,
            class,
            skip_errors,
        }) => {
            run_restore(
                &args.address,
                &input,
                class.as_deref(),
                skip_errors,
                args.tls_cert.as_deref(),
                args.api_key.as_deref(),
            );
        }
        None => {
            // Legacy mode: REPL / single query / file
            let stream = match connect(
                &args.address,
                args.tls_cert.as_deref(),
                args.api_key.as_deref(),
            ) {
                Ok(stream) => stream,
                Err(e) => {
                    eprintln!("Could not connect to {}: {}", args.address, e);
                    eprintln!("Make sure ontodb-server is running.");
                    std::process::exit(1);
                }
            };

            if let Some(query) = args.query {
                run_single_query(stream, &query);
            } else if let Some(file_path) = args.file {
                run_file(stream, &file_path);
            } else {
                run_repl(stream, &args.address);
            }
        }
    }
}

/// Establish a connection to the OntoDB server with optional TLS and API Key.
fn connect(address: &str, tls_cert: Option<&str>, api_key: Option<&str>) -> io::Result<TcpStream> {
    let mut stream = TcpStream::connect(address)?;

    // If TLS certificate is provided, we would upgrade to TLS here
    // For now, TLS support requires native-tls or rustls crate
    if tls_cert.is_some() {
        eprintln!(
            "Warning: TLS support requires additional dependencies. Connection is unencrypted."
        );
        // TODO: Implement TLS upgrade using native-tls or rustls
    }

    // If API key is provided, send authentication
    if let Some(key) = api_key {
        let auth_msg = format!("AUTH {}\n", key);
        stream.write_all(auth_msg.as_bytes())?;
        // Note: Server would need to handle AUTH command
        // For now, this is a placeholder for the authentication protocol
    }

    Ok(stream)
}

// ─────────────────────────────────────────────────────────────
//  Dump subcommand
// ─────────────────────────────────────────────────────────────

fn run_dump(
    address: &str,
    output: Option<&str>,
    class: Option<&str>,
    format: &str,
    tls_cert: Option<&str>,
    api_key: Option<&str>,
) {
    let stream = match connect(address, tls_cert, api_key) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Could not connect to {}: {}", address, e);
            std::process::exit(1);
        }
    };

    // Step 1: Get list of classes
    let classes = if let Some(c) = class {
        vec![c.to_string()]
    } else {
        match get_class_list(&stream) {
            Ok(list) => list,
            Err(e) => {
                eprintln!("Error listing classes: {}", e);
                std::process::exit(1);
            }
        }
    };

    if classes.is_empty() {
        eprintln!("No classes found.");
        return;
    }

    eprintln!("Dumping {} class(es): {:?}", classes.len(), classes);

    // Step 2: Open output
    let mut writer: Box<dyn Write> = match output {
        Some(path) => match std::fs::File::create(path) {
            Ok(f) => Box::new(io::BufWriter::new(f)),
            Err(e) => {
                eprintln!("Error creating '{}': {}", path, e);
                std::process::exit(1);
            }
        },
        None => Box::new(io::stdout()),
    };

    // Step 3: Dump each class
    let mut total_rows = 0u64;
    for cls in &classes {
        let query = format!("SELECT * FROM {}", cls);
        match send_query(&stream, &query) {
            Ok(response) => {
                if response.starts_with("ERR:") {
                    eprintln!("  {}: {}", cls, response);
                    continue;
                }
                let rows = parse_table_response(&response);
                let count = rows.len();
                total_rows += count as u64;

                match format {
                    "csv" => {
                        // CSV: write header + rows
                        if !rows.is_empty() {
                            // Write header from first query (assumed consistent)
                            if total_rows as usize == count {
                                // First class, write CSV header
                            }
                            for row in &rows {
                                let line: Vec<String> = row.iter().map(|v| escape_csv(v)).collect();
                                let _ = writeln!(writer, "{}", line.join(","));
                            }
                        }
                    }
                    _ => {
                        // JSONL (default): one JSON object per line
                        // We need the column names. Parse from response header.
                        let columns = parse_table_columns(&response);
                        for row in &rows {
                            let mut obj = serde_json::Map::new();
                            obj.insert(
                                "__class__".to_string(),
                                serde_json::Value::String(cls.clone()),
                            );
                            for (i, col) in columns.iter().enumerate() {
                                let val = if i < row.len() {
                                    parse_cell_value(&row[i])
                                } else {
                                    serde_json::Value::Null
                                };
                                obj.insert(col.clone(), val);
                            }
                            let line = serde_json::to_string(&obj).unwrap_or_default();
                            let _ = writeln!(writer, "{}", line);
                        }
                    }
                }

                eprintln!("  {}: {} rows", cls, count);
            }
            Err(e) => {
                eprintln!("  {}: Connection error: {}", cls, e);
            }
        }
    }

    eprintln!("Dump complete: {} total rows", total_rows);
}

/// Gets the list of all classes from the server.
fn get_class_list(stream: &TcpStream) -> io::Result<Vec<String>> {
    // Try SELECT * FROM __ontology__ first
    let resp = send_query(stream, "SHOW CLASSES")?;
    if resp.starts_with("ERR:") {
        // Fallback: try to parse from __ontology__
        let resp2 = send_query(stream, "SELECT * FROM __ontology__")?;
        if resp2.starts_with("ERR:") || resp2.contains("(0 rows)") {
            return Ok(Vec::new());
        }
        return Ok(parse_class_list_from_ontology(&resp2));
    }

    // Parse SHOW CLASSES response
    let classes: Vec<String> = resp
        .lines()
        .filter(|l| !l.is_empty() && !l.contains("rows)"))
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty() && !l.starts_with('-'))
        .collect();

    Ok(classes)
}

/// Parses class names from __ontology__ query response.
fn parse_class_list_from_ontology(response: &str) -> Vec<String> {
    let mut classes = Vec::new();
    for line in response.lines() {
        // Each line might contain class info; extract unique class names
        if line.contains("CLASS") || line.contains("class") {
            // Try to extract class name from the line
            let parts: Vec<&str> = line.split('|').map(|s| s.trim()).collect();
            for part in parts {
                let trimmed = part.trim();
                if !trimmed.is_empty()
                    && !trimmed.contains(' ')
                    && !trimmed.starts_with('-')
                    && !trimmed.contains("rows)")
                    && !trimmed.contains("CLASS")
                {
                    classes.push(trimmed.to_string());
                }
            }
        }
    }
    classes.sort();
    classes.dedup();
    classes
}

/// Parses a table response into rows (each row is a Vec of string values).
fn parse_table_response(response: &str) -> Vec<Vec<String>> {
    let lines: Vec<&str> = response.lines().collect();
    let mut rows = Vec::new();

    // Find header line (first non-empty, non-separator line)
    let mut header_found = false;
    let mut num_cols = 0;

    for line in &lines {
        if line.is_empty() {
            continue;
        }
        if line.starts_with('-') {
            continue;
        }
        if line.contains("rows)") {
            continue;
        }

        let cols: Vec<String> = line.split(" | ").map(|s| s.trim().to_string()).collect();

        if !header_found {
            header_found = true;
            num_cols = cols.len();
            continue; // Skip header row
        }

        if cols.len() == num_cols {
            rows.push(cols);
        }
    }

    rows
}

/// Parses column names from the table response header.
fn parse_table_columns(response: &str) -> Vec<String> {
    for line in response.lines() {
        if line.is_empty() || line.starts_with('-') || line.contains("rows)") {
            continue;
        }
        // First non-empty, non-separator line is the header
        return line.split(" | ").map(|s| s.trim().to_string()).collect();
    }
    Vec::new()
}

/// Escapes a value for CSV output.
fn escape_csv(value: &str) -> String {
    if value.contains(',') || value.contains('"') || value.contains('\n') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

/// Parses a cell value string into a serde_json::Value.
fn parse_cell_value(s: &str) -> serde_json::Value {
    let trimmed = s.trim();
    if trimmed == "NULL" || trimmed.is_empty() {
        serde_json::Value::Null
    } else if trimmed == "true" {
        serde_json::Value::Bool(true)
    } else if trimmed == "false" {
        serde_json::Value::Bool(false)
    } else if let Ok(n) = trimmed.parse::<i64>() {
        serde_json::json!(n)
    } else if let Ok(n) = trimmed.parse::<f64>() {
        serde_json::json!(n)
    } else {
        serde_json::Value::String(trimmed.to_string())
    }
}

// ─────────────────────────────────────────────────────────────
//  Restore subcommand
// ─────────────────────────────────────────────────────────────

fn run_restore(
    address: &str,
    input: &str,
    class: Option<&str>,
    skip_errors: bool,
    tls_cert: Option<&str>,
    api_key: Option<&str>,
) {
    let stream = match connect(address, tls_cert, api_key) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Could not connect to {}: {}", address, e);
            std::process::exit(1);
        }
    };

    let file = match std::fs::File::open(input) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("Error opening '{}': {}", input, e);
            std::process::exit(1);
        }
    };

    let reader = io::BufReader::new(file);
    let mut ok = 0u64;
    let mut err = 0u64;
    let mut skipped = 0u64;

    eprintln!("Restoring from {}...", input);

    for (line_no, line) in reader.lines().enumerate() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!("Line {}: Read error: {}", line_no + 1, e);
                err += 1;
                if !skip_errors {
                    break;
                }
                continue;
            }
        };

        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        // Parse JSON line
        let obj: serde_json::Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("Line {}: Invalid JSON: {}", line_no + 1, e);
                err += 1;
                if !skip_errors {
                    break;
                }
                continue;
            }
        };

        // Get class name
        let obj_class = obj
            .get("__class__")
            .and_then(|v| v.as_str())
            .unwrap_or("Unknown");

        // Filter by class if specified
        if let Some(filter_class) = class {
            if obj_class != filter_class {
                skipped += 1;
                continue;
            }
        }

        // Build INSERT statement
        let insert = build_insert_statement(obj_class, &obj);
        match send_query(&stream, &insert) {
            Ok(resp) => {
                if resp.starts_with("ERR:") {
                    eprintln!("Line {}: {}", line_no + 1, resp);
                    err += 1;
                    if !skip_errors {
                        break;
                    }
                } else {
                    ok += 1;
                }
            }
            Err(e) => {
                eprintln!("Line {}: Connection error: {}", line_no + 1, e);
                err += 1;
                if !skip_errors {
                    break;
                }
            }
        }

        // Progress indicator every 1000 rows
        if (ok + err).is_multiple_of(1000) && (ok + err) > 0 {
            eprint!("\r  Progress: {} rows...", ok + err);
        }
    }

    eprintln!();
    eprintln!(
        "Restore complete: {} succeeded, {} failed, {} skipped",
        ok, err, skipped
    );
}

/// Builds an INSERT statement from a JSON object.
fn build_insert_statement(class: &str, obj: &serde_json::Value) -> String {
    let map = match obj.as_object() {
        Some(m) => m,
        None => return format!("INSERT INTO {} VALUES ()", class),
    };

    let mut columns = Vec::new();
    let mut values = Vec::new();

    for (key, val) in map {
        if key == "__class__" {
            continue; // Skip internal field
        }
        columns.push(key.clone());
        values.push(json_value_to_sql(val));
    }

    if columns.is_empty() {
        format!("INSERT INTO {} VALUES ()", class)
    } else {
        format!(
            "INSERT INTO {} ({}) VALUES ({})",
            class,
            columns.join(", "),
            values.join(", ")
        )
    }
}

/// Converts a serde_json::Value to a SQL literal string.
fn json_value_to_sql(val: &serde_json::Value) -> String {
    match val {
        serde_json::Value::Null => "NULL".to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::String(s) => {
            // Escape single quotes
            format!("'{}'", s.replace('\'', "''"))
        }
        serde_json::Value::Array(arr) => {
            let items: Vec<String> = arr.iter().map(json_value_to_sql).collect();
            format!("ARRAY[{}]", items.join(", "))
        }
        serde_json::Value::Object(_) => {
            // Nested object: serialize as JSON string
            format!("'{}'", val.to_string().replace('\'', "''"))
        }
    }
}

// ─────────────────────────────────────────────────────────────
//  Single query mode
// ─────────────────────────────────────────────────────────────

fn run_single_query(stream: TcpStream, query: &str) {
    let query = query.trim().trim_end_matches(';').trim();
    if query.is_empty() {
        return;
    }
    let resp = send_query(&stream, query);
    match resp {
        Ok(r) => {
            if r.starts_with("ERR:") {
                eprintln!("{}", r);
                std::process::exit(1);
            }
            println!("{}", r);
        }
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    }
}

// ─────────────────────────────────────────────────────────────
//  File execution mode
// ─────────────────────────────────────────────────────────────

fn run_file(stream: TcpStream, file_path: &str) {
    let content = match std::fs::read_to_string(file_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error reading '{}': {}", file_path, e);
            std::process::exit(1);
        }
    };

    let mut ok = 0u32;
    let mut err = 0u32;

    for (i, line) in content.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("--") {
            continue;
        }
        let query = line.trim_end_matches(';').trim();
        if query.is_empty() {
            continue;
        }
        match send_query(&stream, query) {
            Ok(resp) => {
                if resp.starts_with("ERR:") {
                    eprintln!("Line {}: {}", i + 1, resp);
                    err += 1;
                } else {
                    if !resp.is_empty() {
                        println!("{}", resp);
                    }
                    ok += 1;
                }
            }
            Err(e) => {
                eprintln!("Line {}: Connection error: {}", i + 1, e);
                err += 1;
            }
        }
    }

    eprintln!("{} succeeded, {} failed", ok, err);
}

// ─────────────────────────────────────────────────────────────
//  Interactive REPL
// ─────────────────────────────────────────────────────────────

fn run_repl(stream: TcpStream, addr: &str) {
    print_banner(addr);

    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let mut line_buf = String::new();
    let mut query_buf = String::new();
    let mut in_query = false;

    loop {
        let prompt = if in_query { "    -> " } else { "ontodb> " };

        print!("{}", prompt);
        if stdout.flush().is_err() {
            break;
        }

        line_buf.clear();
        match stdin.lock().read_line(&mut line_buf) {
            Ok(0) => break,
            Ok(_) => {}
            Err(e) => {
                eprintln!("Read error: {}", e);
                break;
            }
        }

        let trimmed = line_buf.trim();

        if !in_query && trimmed.starts_with('\\') {
            handle_meta_command(trimmed, &stream);
            continue;
        }

        if in_query {
            if !query_buf.is_empty() {
                query_buf.push(' ');
            }
            query_buf.push_str(trimmed);
        } else {
            query_buf.clear();
            query_buf.push_str(trimmed);
        }

        let complete = find_statement_end(&query_buf);
        if let Some(pos) = complete {
            let statement = query_buf[..pos].trim().to_string();
            query_buf.clear();
            in_query = false;

            if statement.is_empty() {
                continue;
            }

            if statement.eq_ignore_ascii_case("quit") || statement.eq_ignore_ascii_case("exit") {
                break;
            }

            execute_with_timing(&stream, &statement);
        } else {
            in_query = true;
        }
    }

    let _ = send_query(&stream, "quit");
    println!();
    eprintln!("Bye!");
}

fn find_statement_end(input: &str) -> Option<usize> {
    let mut in_quote: Option<char> = None;
    for (i, c) in input.char_indices() {
        match in_quote {
            Some(q) if c == q => in_quote = None,
            Some(_) => {}
            None if c == '\'' || c == '"' => in_quote = Some(c),
            None if c == ';' => return Some(i),
            None => {}
        }
    }
    None
}

fn execute_with_timing(stream: &TcpStream, query: &str) {
    let start = Instant::now();

    match send_query(stream, query) {
        Ok(response) => {
            let elapsed = start.elapsed();

            if response.starts_with("ERR:") {
                eprintln!("{}", response);
            } else if response.is_empty() {
            } else {
                if response.contains("(0 rows)") {
                    println!("{}", response);
                } else if response.contains("rows)") {
                    print_table(&response);
                } else {
                    println!("{}", response);
                }
            }

            if elapsed.as_millis() > 0 {
                eprintln!("({:.3}s)", elapsed.as_secs_f64());
            }
        }
        Err(e) => {
            eprintln!("Connection error: {}", e);
        }
    }
}

fn print_table(raw: &str) {
    let lines: Vec<&str> = raw.lines().collect();
    if lines.is_empty() {
        return;
    }

    let header = lines[0];
    let header_cols: Vec<&str> = header.split(" | ").map(|s| s.trim()).collect();
    let num_cols = header_cols.len();

    let mut data_rows: Vec<Vec<String>> = Vec::new();
    let mut count_line = "";

    for line in &lines[1..] {
        if line.starts_with('-') {
            continue;
        }
        if line.contains("rows)") {
            count_line = line;
            continue;
        }
        let cols: Vec<String> = line.split(" | ").map(|s| s.trim().to_string()).collect();
        if cols.len() == num_cols {
            data_rows.push(cols);
        }
    }

    let mut widths: Vec<usize> = header_cols.iter().map(|c| c.len()).collect();
    for row in &data_rows {
        for (i, col) in row.iter().enumerate() {
            if i < widths.len() {
                widths[i] = widths[i].max(col.len());
            }
        }
    }

    let sep: String = widths
        .iter()
        .map(|w| "-".repeat(*w + 2))
        .collect::<Vec<_>>()
        .join("-+-");

    let header_line: String = header_cols
        .iter()
        .enumerate()
        .map(|(i, c)| format!(" {:<width$} ", c, width = widths[i]))
        .collect::<Vec<_>>()
        .join("|");
    println!("{}", header_line);
    println!("{}", sep);

    for row in &data_rows {
        let row_line: String = row
            .iter()
            .enumerate()
            .map(|(i, c)| {
                if i < widths.len() {
                    format!(" {:<width$} ", c, width = widths[i])
                } else {
                    format!(" {} ", c)
                }
            })
            .collect::<Vec<_>>()
            .join("|");
        println!("{}", row_line);
    }

    if !count_line.is_empty() {
        println!("{}", count_line);
    }
}

fn handle_meta_command(cmd: &str, stream: &TcpStream) {
    let stream = match stream.try_clone() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error: failed to clone stream: {}", e);
            return;
        }
    };
    match cmd {
        "\\?" | "\\help" => {
            print_help();
        }
        "\\q" | "\\quit" | "\\exit" => {
            let _ = send_query(&stream, "quit");
            println!();
            eprintln!("Bye!");
            std::process::exit(0);
        }
        "\\d" | "\\dt" => {
            let resp = send_query(&stream, "SELECT * FROM __ontology__").unwrap_or_default();
            if resp.contains("(0 rows)") || resp.starts_with("ERR:") {
                println!("No ontologies found.");
            } else {
                println!("{}", resp);
            }
        }
        "\\version" => {
            println!("OntoDB CLI v{}", env!("CARGO_PKG_VERSION"));
        }
        "\\clear" | "\\cls" => {
            print!("\x1B[2J\x1B[H");
        }
        _ if cmd.starts_with("\\c ") => {
            eprintln!("Reconnect not yet supported. Restart the CLI with a new address.");
        }
        _ => {
            eprintln!("Unknown command: {}", cmd);
            eprintln!("Type \\help for available commands.");
        }
    }
}

fn print_banner(addr: &str) {
    println!("OntoDB CLI v{}", env!("CARGO_PKG_VERSION"));
    println!("Connected to {}", addr);
    println!();
    println!("Type SQL queries ending with ';' to execute.");
    println!("Use \\help for available commands, \\q to quit.");
    println!();
}

fn print_help() {
    println!("Available commands:");
    println!();
    println!("  SQL statements (end with ';'):");
    println!("    CREATE ONTOLOGY <name> (...)    Create an ontology");
    println!("    INSERT INTO <class> ...         Insert data");
    println!("    SELECT ... FROM <class> ...     Query data");
    println!("    UPDATE <class> SET ...          Update data");
    println!("    DELETE FROM <class> ...         Delete data");
    println!("    MATCH (v: <class>) ...          Semantic query");
    println!();
    println!("  Special commands:");
    println!("    \\help  \\?     Show this help");
    println!("    \\d  \\dt       List ontologies/classes");
    println!("    \\version      Show CLI version");
    println!("    \\clear        Clear screen");
    println!("    \\q            Quit");
    println!();
    println!("  Subcommands:");
    println!("    ontodb-cli dump    -o backup.jsonl         Dump all data");
    println!("    ontodb-cli dump    -c BioTask -o task.jsonl Dump one class");
    println!("    ontodb-cli restore -i backup.jsonl         Restore data");
    println!();
    println!("  Multi-line input:");
    println!("    Statements can span multiple lines.");
    println!("    End with ';' to execute.");
    println!();
    println!("  Examples:");
    println!("    CREATE ONTOLOGY shop (");
    println!("      CLASS Product,");
    println!("      PROPERTY name DOMAIN Product RANGE STRING");
    println!("    );");
    println!();
    println!("    SELECT * FROM Product WHERE price > 100;");
    println!();
}

// ─────────────────────────────────────────────────────────────
//  Protocol helpers
// ─────────────────────────────────────────────────────────────

fn send_query(stream: &TcpStream, query: &str) -> io::Result<String> {
    let mut stream = stream.try_clone()?;
    stream.write_all(query.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()?;

    let mut reader = BufReader::new(stream);
    let mut response = Vec::new();
    let mut byte = [0u8; 1];

    loop {
        match reader.read_exact(&mut byte) {
            Ok(()) => {
                if byte[0] == 0 {
                    break;
                }
                response.push(byte[0]);
            }
            Err(e) => return Err(e),
        }
    }

    String::from_utf8(response).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_statement_end_simple() {
        assert_eq!(find_statement_end("SELECT 1;"), Some(8));
        assert_eq!(find_statement_end("SELECT 1"), None);
        assert_eq!(find_statement_end(""), None);
    }

    #[test]
    fn test_find_statement_end_with_quotes() {
        assert_eq!(find_statement_end("SELECT 'hello;world';"), Some(20));
        assert_eq!(find_statement_end(r#"SELECT "hello;world";"#), Some(20));
    }

    #[test]
    fn test_find_statement_end_multiple_semicolons() {
        assert_eq!(find_statement_end("SELECT 1; SELECT 2;"), Some(8));
    }

    #[test]
    fn test_find_statement_end_empty_quotes() {
        assert_eq!(find_statement_end("'';"), Some(2));
        assert_eq!(find_statement_end(r#""";"#), Some(2));
    }

    #[test]
    fn test_build_insert_statement() {
        let obj = serde_json::json!({
            "__class__": "Product",
            "name": "Test Product",
            "price": 99.5,
            "active": true
        });
        let stmt = build_insert_statement("Product", &obj);
        assert!(stmt.starts_with("INSERT INTO Product"));
        assert!(stmt.contains("name"));
        assert!(stmt.contains("price"));
        assert!(stmt.contains("active"));
        assert!(!stmt.contains("__class__"));
    }

    #[test]
    fn test_json_value_to_sql() {
        assert_eq!(json_value_to_sql(&serde_json::Value::Null), "NULL");
        assert_eq!(json_value_to_sql(&serde_json::json!(42)), "42");
        assert_eq!(json_value_to_sql(&serde_json::json!(3.14)), "3.14");
        assert_eq!(json_value_to_sql(&serde_json::json!(true)), "true");
        assert_eq!(json_value_to_sql(&serde_json::json!("hello")), "'hello'");
        assert_eq!(json_value_to_sql(&serde_json::json!("it's")), "'it''s'");
    }

    #[test]
    fn test_escape_csv() {
        assert_eq!(escape_csv("hello"), "hello");
        assert_eq!(escape_csv("hello,world"), "\"hello,world\"");
        assert_eq!(escape_csv("say \"hi\""), "\"say \"\"hi\"\"\"");
    }

    #[test]
    fn test_parse_table_response() {
        let response = "name | age\n-------\nAlice | 30\nBob | 25\n(2 rows)";
        let rows = parse_table_response(response);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], vec!["Alice", "30"]);
        assert_eq!(rows[1], vec!["Bob", "25"]);
    }

    #[test]
    fn test_parse_table_columns() {
        let response = "name | age\n-------\nAlice | 30\n(1 row)";
        let cols = parse_table_columns(response);
        assert_eq!(cols, vec!["name", "age"]);
    }

    #[test]
    fn test_parse_cell_value() {
        assert_eq!(parse_cell_value("NULL"), serde_json::Value::Null);
        assert_eq!(parse_cell_value("true"), serde_json::json!(true));
        assert_eq!(parse_cell_value("42"), serde_json::json!(42));
        assert_eq!(parse_cell_value("3.14"), serde_json::json!(3.14));
        assert_eq!(parse_cell_value("hello"), serde_json::json!("hello"));
    }
}

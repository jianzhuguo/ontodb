//! OntoDB CLI - Interactive command-line client for OntoDB server.
//!
//! Features:
//! - Interactive REPL with multi-line input (end statements with `;`)
//! - Special commands: \help, \d, \q, \c
//! - Query timing
//! - Aligned column output with borders
//! - Single-query mode (-q) and file mode (-f)

use clap::Parser;
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
          ontodb-cli -f init.sql                  # execute SQL file"
)]
struct Args {
    /// Server address (host:port)
    #[arg(default_value = "127.0.0.1:7913")]
    address: String,

    /// Execute a single query and exit
    #[arg(short, long)]
    query: Option<String>,

    /// Execute queries from a SQL file
    #[arg(short, long)]
    file: Option<String>,
}

fn main() {
    let args = Args::parse();

    // Connect to server
    let stream = match TcpStream::connect(&args.address) {
        Ok(stream) => {
            stream
        }
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

// ═══════════════════════════════════════════════════════════════════
//  Single query mode
// ═══════════════════════════════════════════════════════════════════

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

// ═══════════════════════════════════════════════════════════════════
//  File execution mode
// ═══════════════════════════════════════════════════════════════════

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

// ═══════════════════════════════════════════════════════════════════
//  Interactive REPL
// ═══════════════════════════════════════════════════════════════════

fn run_repl(stream: TcpStream, addr: &str) {
    print_banner(addr);

    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let mut line_buf = String::new();
    let mut query_buf = String::new(); // accumulates multi-line input
    let mut in_query = false; // true when accumulating a multi-line statement

    loop {
        let prompt = if in_query {
            "    -> " // continuation prompt
        } else {
            "ontodb> "
        };

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

        // Handle special commands (only at start of input, not inside multi-line)
        if !in_query && trimmed.starts_with('\\') {
            handle_meta_command(trimmed, &stream);
            continue;
        }

        // Accumulate input
        if in_query {
            if !query_buf.is_empty() {
                query_buf.push(' ');
            }
            query_buf.push_str(trimmed);
        } else {
            query_buf.clear();
            query_buf.push_str(trimmed);
        }

        // Check if statement is complete (ends with ';')
        let complete = find_statement_end(&query_buf);
        if let Some(pos) = complete {
            let statement = query_buf[..pos].trim().to_string();
            query_buf.clear();
            in_query = false;

            if statement.is_empty() {
                continue;
            }

            // Handle quit/exit
            if statement.eq_ignore_ascii_case("quit") || statement.eq_ignore_ascii_case("exit") {
                break;
            }

            // Execute
            execute_with_timing(&stream, &statement);
        } else {
            // Statement not complete, continue accumulating
            in_query = true;
        }
    }

    // Clean disconnect
    let _ = send_query(&stream, "quit");
    println!();
    eprintln!("Bye!");
}

/// Finds the position of the statement terminator (`;`), skipping quoted content.
/// Returns the index of the `;` that ends the statement, or None if not found.
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

/// Executes a query with timing and formatted output.
fn execute_with_timing(stream: &TcpStream, query: &str) {
    let start = Instant::now();

    match send_query(stream, query) {
        Ok(response) => {
            let elapsed = start.elapsed();

            if response.starts_with("ERR:") {
                eprintln!("{}", response);
            } else if response.is_empty() {
                // Empty response (e.g., for empty input)
            } else {
                // Check if it's a row result (contains "|" or "(N rows)")
                if response.contains("(0 rows)") {
                    println!("{}", response);
                } else if response.contains("rows)") {
                    // It's a SELECT result — format with borders
                    print_table(&response);
                } else {
                    // It's a success message (INSERT, UPDATE, DELETE, CREATE)
                    println!("{}", response);
                }
            }

            // Show timing for non-trivial operations
            if elapsed.as_millis() > 0 {
                eprintln!("({:.3}s)", elapsed.as_secs_f64());
            }
        }
        Err(e) => {
            eprintln!("Connection error: {}", e);
        }
    }
}

/// Prints a table with proper column alignment and borders.
fn print_table(raw: &str) {
    let lines: Vec<&str> = raw.lines().collect();
    if lines.is_empty() {
        return;
    }

    // Parse header (first line, pipe-separated)
    let header = lines[0];
    let header_cols: Vec<&str> = header.split(" | ").map(|s| s.trim()).collect();
    let num_cols = header_cols.len();

    // Parse data rows (between header and separator/count line)
    let mut data_rows: Vec<Vec<String>> = Vec::new();
    let mut count_line = "";

    for line in &lines[1..] {
        if line.starts_with('-') {
            continue; // separator line
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

    // Calculate column widths
    let mut widths: Vec<usize> = header_cols.iter().map(|c| c.len()).collect();
    for row in &data_rows {
        for (i, col) in row.iter().enumerate() {
            if i < widths.len() {
                widths[i] = widths[i].max(col.len());
            }
        }
    }

    // Build separator line
    let sep: String = widths
        .iter()
        .map(|w| "-".repeat(*w + 2))
        .collect::<Vec<_>>()
        .join("-+-");

    // Print header
    let header_line: String = header_cols
        .iter()
        .enumerate()
        .map(|(i, c)| format!(" {:<width$} ", c, width = widths[i]))
        .collect::<Vec<_>>()
        .join("|");
    println!("{}", header_line);
    println!("{}", sep);

    // Print rows
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

    // Print count
    if !count_line.is_empty() {
        println!("{}", count_line);
    }
}

/// Handles backslash meta-commands.
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
            // List all ontologies/classes by querying the ontology store
            // We do this by trying to SELECT from __ontology__ keys
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
            // ANSI clear screen
            print!("\x1B[2J\x1B[H");
        }
        _ if cmd.starts_with("\\c ") => {
            // Connect to a different server (future feature)
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

// ═══════════════════════════════════════════════════════════════════
//  Protocol helpers
// ═══════════════════════════════════════════════════════════════════

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
        // Semicolon inside single quotes should be ignored
        assert_eq!(find_statement_end("SELECT 'hello;world';"), Some(20));
        // Semicolon inside double quotes should be ignored
        assert_eq!(find_statement_end(r#"SELECT "hello;world";"#), Some(20));
    }

    #[test]
    fn test_find_statement_end_multiple_semicolons() {
        // Returns first unquoted semicolon
        assert_eq!(find_statement_end("SELECT 1; SELECT 2;"), Some(8));
    }

    #[test]
    fn test_find_statement_end_empty_quotes() {
        assert_eq!(find_statement_end("'';"), Some(2));
        assert_eq!(find_statement_end(r#""";"#), Some(2));
    }
}

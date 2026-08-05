//! OntoDB CLI - Command-line client for connecting to OntoDB server.
//!
//! Connects to a running ontodb-server via TCP and sends SQL queries.
//! Supports interactive REPL mode and single-query mode.

use clap::Parser;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::TcpStream;

#[derive(Parser, Debug)]
#[command(
    name = "ontodb-cli",
    about = "OntoDB command-line client",
    long_about = "Connect to an OntoDB server and execute queries.\n\nExamples:\n  ontodb-cli                          # connect to localhost:6500\n  ontodb-cli 192.168.1.100:6500       # connect to remote server\n  ontodb-cli -q \"SELECT * FROM Product\" # execute single query\n  ontodb-cli -f script.sql            # execute queries from file"
)]
struct Args {
    /// Server address (host:port)
    #[arg(default_value = "127.0.0.1:6500")]
    address: String,

    /// Execute a single query and exit
    #[arg(short, long)]
    query: Option<String>,

    /// Execute queries from a file
    #[arg(short, long)]
    file: Option<String>,
}

fn main() {
    let args = Args::parse();

    println!("OntoDB CLI v{}", env!("CARGO_PKG_VERSION"));

    // Connect to server
    let stream = match TcpStream::connect(&args.address) {
        Ok(stream) => {
            println!("Connected to {}", args.address);
            stream
        }
        Err(e) => {
            eprintln!("Error: Could not connect to {}: {}", args.address, e);
            eprintln!("Make sure ontodb-server is running on that address.");
            std::process::exit(1);
        }
    };

    if let Some(query) = args.query {
        // Single query mode
        run_single_query(stream, &query);
    } else if let Some(file_path) = args.file {
        // File execution mode
        run_file(stream, &file_path);
    } else {
        // Interactive REPL mode
        run_repl(stream);
    }
}

/// Executes a single query and prints the result.
fn run_single_query(mut stream: TcpStream, query: &str) {
    let query = query.trim().trim_end_matches(';').trim();
    if let Err(e) = send_query(&mut stream, query) {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }
    match read_response(&stream) {
        Ok(response) => println!("{}", response),
        Err(e) => {
            eprintln!("Error reading response: {}", e);
            std::process::exit(1);
        }
    }
}

/// Executes queries from a SQL file.
fn run_file(mut stream: TcpStream, file_path: &str) {
    let content = match std::fs::read_to_string(file_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error reading file '{}': {}", file_path, e);
            std::process::exit(1);
        }
    };

    for (i, line) in content.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("--") {
            continue;
        }

        let query = line.trim_end_matches(';').trim();
        if query.is_empty() {
            continue;
        }

        if let Err(e) = send_query(&mut stream, query) {
            eprintln!("Error on line {}: {}", i + 1, e);
            continue;
        }

        match read_response(&stream) {
            Ok(response) => {
                if !response.is_empty() {
                    println!("{}", response);
                }
            }
            Err(e) => {
                eprintln!("Error reading response on line {}: {}", i + 1, e);
                break;
            }
        }
    }
}

/// Runs the interactive REPL.
fn run_repl(mut stream: TcpStream) {
    println!("Type 'quit' or 'exit' to disconnect. End statements with ';'.");
    println!();

    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let mut buffer = String::new();

    loop {
        print!("ontodb> ");
        if stdout.flush().is_err() {
            break;
        }

        buffer.clear();
        match stdin.lock().read_line(&mut buffer) {
            Ok(0) => break, // EOF
            Ok(_) => {}
            Err(e) => {
                eprintln!("Read error: {}", e);
                break;
            }
        }

        let input = buffer.trim();
        if input.is_empty() {
            continue;
        }
        if input.eq_ignore_ascii_case("quit") || input.eq_ignore_ascii_case("exit") {
            break;
        }

        let query = input.trim_end_matches(';').trim();
        if query.is_empty() {
            continue;
        }

        if let Err(e) = send_query(&mut stream, query) {
            eprintln!("Error: {}", e);
            break;
        }

        match read_response(&stream) {
            Ok(response) => {
                if !response.is_empty() {
                    println!("{}", response);
                    println!();
                }
            }
            Err(e) => {
                eprintln!("Error: {}", e);
                break;
            }
        }
    }

    // Send quit to server
    let _ = send_query(&mut stream, "quit");
    println!("Disconnected.");
}

/// Sends a query to the server (newline-terminated).
fn send_query(stream: &mut TcpStream, query: &str) -> io::Result<()> {
    stream.write_all(query.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()
}

/// Reads a response from the server (null-byte terminated).
fn read_response(stream: &TcpStream) -> io::Result<String> {
    let mut reader = BufReader::new(stream);
    let mut response = Vec::new();
    let mut byte = [0u8; 1];

    loop {
        match reader.read_exact(&mut byte) {
            Ok(()) => {
                if byte[0] == 0 {
                    break; // End of message
                }
                response.push(byte[0]);
            }
            Err(e) => return Err(e),
        }
    }

    String::from_utf8(response).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

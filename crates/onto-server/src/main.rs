//! OntoDB Server - Main entry point.
//!
//! Supports two modes:
//! - Standalone REPL (interactive or stdin)
//! - TCP server (accepts multiple CLI connections)

use clap::Parser;
use onto_core::Result;
use onto_ontology::OntologyStore;
use onto_query::{QueryExecutor, QueryParser};
use onto_storage::{LsmEngine, StorageOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::thread;

#[derive(Parser, Debug)]
#[command(name = "ontodb-server", about = "OntoDB - Ontology-driven semantic database")]
struct Args {
    /// Data directory path
    #[arg(short, long, default_value = "./ontodb_data")]
    data_dir: PathBuf,

    /// MemTable size limit in bytes
    #[arg(short, long, default_value = "4194304")]
    memtable_size: usize,

    /// Run in interactive mode (REPL)
    #[arg(short, long)]
    interactive: bool,

    /// TCP listen address (enables server mode)
    #[arg(short = 'l', long, default_value = "127.0.0.1:6500")]
    listen: String,
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
        // Start TCP server
        run_tcp_server(&args.listen, executor)?;
    }

    println!("Goodbye.");
    Ok(())
}

/// Runs the TCP server, accepting client connections.
fn run_tcp_server(addr: &str, executor: Arc<QueryExecutor>) -> Result<()> {
    let listener = TcpListener::bind(addr)
        .map_err(|e| onto_core::CoreError::Io(e))?;

    println!("Listening on {}", addr);
    println!("Connect with: ontodb-cli {}", addr);

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let executor = Arc::clone(&executor);
                thread::spawn(move || {
                    if let Err(e) = handle_client(stream, &executor) {
                        eprintln!("Client error: {}", e);
                    }
                });
            }
            Err(e) => {
                eprintln!("Connection error: {}", e);
            }
        }
    }

    Ok(())
}

/// Handles a single client connection.
///
/// Protocol:
/// - Client sends SQL queries, one per line (terminated by `\n`)
/// - Server executes and sends back formatted results
/// - Result is terminated by a null byte (`\0`) as end-of-message marker
/// - Errors are prefixed with `ERR: `
/// - Client sends `quit` or `exit` to disconnect
fn handle_client(stream: TcpStream, executor: &QueryExecutor) -> Result<()> {
    let peer = stream.peer_addr().map(|a| a.to_string()).unwrap_or_default();
    println!("Client connected: {}", peer);

    let reader = BufReader::new(stream.try_clone()?);
    let mut writer = stream;

    for line in reader.lines() {
        let line = line.map_err(|e| onto_core::CoreError::Io(e))?;
        let input = line.trim();

        if input.is_empty() {
            // Send empty response
            writer.write_all(&[0])?;
            continue;
        }

        if input.eq_ignore_ascii_case("quit") || input.eq_ignore_ascii_case("exit") {
            break;
        }

        // Remove trailing semicolon
        let input = input.trim_end_matches(';').trim();

        // Execute query
        let response = match QueryParser::parse(input) {
            Ok(ast) => match executor.execute(&ast) {
                Ok(result) => result.format(),
                Err(e) => format!("ERR: {}", e),
            },
            Err(e) => format!("ERR: Parse error: {}", e),
        };

        // Send response + null terminator
        writer.write_all(response.as_bytes())?;
        writer.write_all(&[0])?;
        writer.flush()?;
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
        Ok(ast) => match executor.execute(&ast) {
            Ok(result) => {
                println!("{}", result.format());
                println!();
            }
            Err(e) => {
                eprintln!("Error: {}", e);
            }
        },
        Err(e) => {
            eprintln!("Parse error: {}", e);
        }
    }
}

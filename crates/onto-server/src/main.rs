//! OntoDB Server - Main entry point.

use clap::Parser;
use onto_core::Result;
use onto_ontology::OntologyStore;
use onto_query::{QueryExecutor, QueryParser};
use onto_storage::{LsmEngine, StorageOptions};
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

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
    let executor = QueryExecutor::new(Arc::clone(&engine), ontology_store);

    if args.interactive {
        run_repl(&executor)?;
    } else {
        println!("Server started. Use --interactive for REPL mode.");
        println!("Listening for queries on stdin...");

        let stdin = io::stdin();
        for line in stdin.lock().lines() {
            let line = line?;
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if line.eq_ignore_ascii_case("quit") || line.eq_ignore_ascii_case("exit") {
                break;
            }

            execute_and_print(&executor, line);
        }
    }

    println!("Goodbye.");
    Ok(())
}

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
            break; // EOF
        }

        let input = buffer.trim();
        if input.is_empty() {
            continue;
        }
        if input.eq_ignore_ascii_case("quit") || input.eq_ignore_ascii_case("exit") {
            break;
        }

        // Remove trailing semicolon
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

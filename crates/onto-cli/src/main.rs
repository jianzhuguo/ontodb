//! OntoDB CLI - Command-line interface.

use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "ontodb-cli", about = "OntoDB command-line client")]
struct Args {
    /// Server address
    #[arg(default_value = "localhost:5432")]
    address: String,
}

fn main() {
    println!("OntoDB CLI v{}", env!("CARGO_PKG_VERSION"));
    println!("Connecting to {}...", "local");

    // For now, this is a placeholder.
    // The real CLI will connect to the server via gRPC/TCP.
    println!("CLI not yet implemented. Use ontodb-server --interactive for now.");
}

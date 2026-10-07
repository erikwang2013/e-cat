// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "ecat")]
#[command(version, about = "e-cat microservices framework CLI", long_about = None)]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) command: Commands,
}

#[derive(Subcommand)]
pub(crate) enum Commands {
    /// Create a new e-cat project
    New {
        /// Project name
        name: String,
    },
    /// Manage protobuf files
    Proto {
        #[command(subcommand)]
        action: ProtoAction,
    },
    /// Run the project in development mode
    Run {
        /// Restart on source changes
        #[arg(long)]
        watch: bool,
    },
    /// Build the project for production
    Build {
        /// Build in release mode
        #[arg(long)]
        release: bool,
    },
    /// Update all ecat-* workspace dependencies
    Upgrade,
}

#[derive(Subcommand)]
pub(crate) enum ProtoAction {
    /// Add a proto file to the project
    Add {
        /// Path to the proto file
        file: String,
    },
    /// Generate client code from proto
    Client {
        /// Path to the proto file
        file: String,
    },
    /// Generate server code from proto
    Server {
        /// Path to the proto file
        file: String,
        /// Output directory for generated server code
        #[arg(short = 't', long)]
        output: Option<String>,
    },
}

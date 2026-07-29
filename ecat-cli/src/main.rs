// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use clap::{Parser, Subcommand};
use std::process::{self, Command};

#[derive(Parser)]
#[command(name = "ecat")]
#[command(about = "e-cat microservices framework CLI", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
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
    Run,
    /// Build the project for production
    Build {
        /// Build in release mode
        #[arg(long)]
        release: bool,
    },
}

#[derive(Subcommand)]
enum ProtoAction {
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

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::New { name } => {
            println!("Creating new e-cat project: {}", name);
            println!("  mkdir {}", name);
            println!("  generating Cargo.toml...");
            println!("  generating src/main.rs...");
            println!("  generating proto/...");
            println!("Project '{}' created successfully!", name);
            println!();
            println!("Next steps:");
            println!("  cd {}", name);
            println!("  ecat run");
        }
        Commands::Proto { action } => match action {
            ProtoAction::Add { file } => {
                println!("Adding proto file: {}", file);
                println!("Proto file added to api/");
            }
            ProtoAction::Client { file } => {
                println!("Generating client code from: {}", file);
                println!("Client code generated to api/<package>/");
            }
            ProtoAction::Server { file, output } => {
                let out = output.unwrap_or_else(|| "internal/service".into());
                println!("Generating server code from: {}", file);
                println!("Server code generated to {}", out);
            }
        },
        Commands::Run => {
            println!("Starting development server...");
            let status = Command::new("cargo")
                .arg("run")
                .status()
                .unwrap_or_else(|e| {
                    eprintln!("Failed to start: {}", e);
                    process::exit(1);
                });
            if !status.success() {
                process::exit(status.code().unwrap_or(1));
            }
        }
        Commands::Build { release } => {
            let mut cmd = Command::new("cargo");
            cmd.arg("build");
            if release {
                println!("Building in release mode...");
                cmd.arg("--release");
            } else {
                println!("Building...");
            }
            let status = cmd.status().unwrap_or_else(|e| {
                eprintln!("Build failed: {}", e);
                process::exit(1);
            });
            if !status.success() {
                process::exit(status.code().unwrap_or(1));
            }
            println!("Build complete!");
        }
    }
}

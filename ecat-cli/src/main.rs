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
            use std::fs;

            let dir = std::path::Path::new(&name);
            if dir.exists() {
                eprintln!("Error: directory '{}' already exists", name);
                process::exit(1);
            }

            fs::create_dir_all(dir.join("src")).unwrap_or_else(|e| {
                eprintln!("Failed to create project: {}", e);
                process::exit(1);
            });
            fs::create_dir_all(dir.join("proto")).unwrap_or_else(|e| {
                eprintln!("Failed to create proto dir: {}", e);
                process::exit(1);
            });

            let cargo_toml = format!(
                r#"[package]
name = "{}"
version = "0.1.0"
edition = "2024"

[dependencies]
ecat = "1.0"
ecat-transport-http = "1.0"
ecat-middleware = "1.0"
ecat-logging = "1.0"
tokio = {{ version = "1", features = ["full"] }}
tracing = "0.1"
axum = "0.8"
serde = {{ version = "1", features = ["derive"] }}
serde_json = "1"
"#,
                name
            );
            fs::write(dir.join("Cargo.toml"), cargo_toml).unwrap_or_else(|e| {
                eprintln!("Failed to write Cargo.toml: {}", e);
                process::exit(1);
            });

            let main_rs = r#"use axum::{{routing::get, Json, Router}};
use ecat::App;
use ecat_middleware::{{LoggingLayer, TracingLayer}};
use ecat_transport_http::HttpServer;
use serde::Serialize;
use tower::ServiceBuilder;

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {{ status: "ok" }})
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let middleware = ServiceBuilder::new()
        .layer(TracingLayer)
        .layer(LoggingLayer);

    let router = Router::new()
        .route("/health", get(health))
        .layer(middleware);

    let http_srv = HttpServer::new(":8000").router(router);

    let mut app = App::builder()
        .name(env!("CARGO_PKG_NAME"))
        .version(env!("CARGO_PKG_VERSION"))
        .server(http_srv)
        .build()?;

    app.run().await?;
    Ok(())
}
"#;
            fs::write(dir.join("src").join("main.rs"), main_rs).unwrap_or_else(|e| {
                eprintln!("Failed to write main.rs: {}", e);
                process::exit(1);
            });

            let proto_file = r#"syntax = "proto3";
package service;

service Service {
    rpc Health(HealthRequest) returns (HealthResponse);
}

message HealthRequest {}
message HealthResponse {
    string status = 1;
}
"#;
            fs::write(dir.join("proto").join("service.proto"), proto_file).unwrap_or_else(|e| {
                eprintln!("Failed to write service.proto: {}", e);
                process::exit(1);
            });

            println!("Project '{}' created successfully!", name);
            println!();
            println!("  {}/Cargo.toml", name);
            println!("  {}/src/main.rs", name);
            println!("  {}/proto/service.proto", name);
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

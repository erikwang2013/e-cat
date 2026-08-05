// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use clap::{Parser, Subcommand};
use std::process::{self, Command};
use std::sync::mpsc;
use std::time::Duration;

#[derive(Parser)]
#[command(name = "ecat")]
#[command(version, about = "e-cat microservices framework CLI", long_about = None)]
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

            let cargo_toml = ecat_cli::generate_cargo_toml(&name);
            fs::write(dir.join("Cargo.toml"), cargo_toml).unwrap_or_else(|e| {
                eprintln!("Failed to write Cargo.toml: {}", e);
                process::exit(1);
            });

            let main_rs = ecat_cli::generate_main_rs();
            fs::write(dir.join("src").join("main.rs"), main_rs).unwrap_or_else(|e| {
                eprintln!("Failed to write main.rs: {}", e);
                process::exit(1);
            });

            let proto_file = ecat_cli::generate_proto_file();
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
        Commands::Run { watch } => {
            if watch {
                run_watch();
            } else {
                run_cargo_run();
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
        Commands::Upgrade => upgrade_packages(),
    }
}

fn run_cargo_run() {
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

fn upgrade_packages() {
    let out = Command::new("cargo")
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .output()
        .unwrap_or_else(|e| {
            eprintln!("Failed to read workspace metadata: {}", e);
            process::exit(1);
        });
    if !out.status.success() {
        eprintln!("cargo metadata failed");
        process::exit(out.status.code().unwrap_or(1));
    }
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        eprintln!("Failed to parse metadata: {}", e);
        process::exit(1);
    });
    let mut names: Vec<String> = json["packages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| p["name"].as_str())
        .filter(|n| n.starts_with("ecat-"))
        .map(str::to_string)
        .collect();
    names.sort();
    if names.is_empty() {
        println!("No ecat-* packages found in this workspace");
        return;
    }
    for name in &names {
        println!("Updating {}...", name);
        let status = Command::new("cargo")
            .args(["update", "-p", name])
            .status()
            .unwrap_or_else(|e| {
                eprintln!("Failed to run cargo update: {}", e);
                process::exit(1);
            });
        if !status.success() {
            eprintln!("cargo update failed for {}", name);
        }
    }
    println!("Updated {} packages", names.len());
}

fn run_watch() {
    use notify::{Config, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

    let (tx, rx) = mpsc::channel::<()>();
    let mut watcher = RecommendedWatcher::new(
        move |res: notify::Result<notify::Event>| {
            if let Ok(event) = res {
                let relevant = matches!(
                    event.kind,
                    EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
                );
                if relevant {
                    tx.send(()).ok();
                }
            }
        },
        Config::default(),
    )
    .unwrap_or_else(|e| {
        eprintln!("Failed to create file watcher: {}", e);
        process::exit(1);
    });
    watcher
        .watch(std::path::Path::new("src"), RecursiveMode::Recursive)
        .unwrap_or_else(|e| {
            eprintln!("Failed to watch src/: {}", e);
            process::exit(1);
        });

    println!("Watching src/ for changes (Ctrl-C to stop)...");
    let mut child = Command::new("cargo")
        .arg("run")
        .spawn()
        .unwrap_or_else(|e| {
            eprintln!("Failed to start: {}", e);
            process::exit(1);
        });
    loop {
        if rx.recv().is_err() {
            break;
        }
        // debounce: only restart after 500ms of silence
        while rx.recv_timeout(Duration::from_millis(500)).is_ok() {}
        println!("\nChange detected, restarting...");
        let _ = child.kill();
        let _ = child.wait();
        child = Command::new("cargo")
            .arg("run")
            .spawn()
            .unwrap_or_else(|e| {
                eprintln!("Failed to restart: {}", e);
                process::exit(1);
            });
    }
}

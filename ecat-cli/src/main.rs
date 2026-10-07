// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use clap::Parser;
use std::process::{self, Command};

mod cli;
mod project;
mod proto;
mod upgrade;
mod watch;

use cli::{Cli, Commands, ProtoAction};

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::New { name } => project::create_project(name),
        Commands::Proto { action } => match action {
            ProtoAction::Add { file } => proto::proto_add(&file),
            ProtoAction::Client { file } => proto::proto_generate(&file, false, None),
            ProtoAction::Server { file, output } => {
                proto::proto_generate(&file, true, output.as_deref())
            }
        },
        Commands::Run { watch: watch_mode } => {
            if watch_mode {
                watch::run_watch();
            } else {
                watch::run_cargo_run();
            }
        }
        Commands::Build { release } => build(release),
        Commands::Upgrade => upgrade::upgrade_packages(),
    }
}

/// `ecat build`：在项目目录里调用 cargo build，并透传子进程退出码。
fn build(release: bool) {
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

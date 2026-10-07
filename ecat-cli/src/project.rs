// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use std::fs;
use std::process;

/// `ecat new`：生成项目骨架（Cargo.toml / src/main.rs / proto/service.proto）。
pub(crate) fn create_project(name: String) {
    if let Err(msg) = ecat_cli::validate_crate_name(&name) {
        eprintln!("Error: invalid project name '{}': {}", name, msg);
        process::exit(1);
    }

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

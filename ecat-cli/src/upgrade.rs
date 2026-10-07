// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use std::fs;
use std::process::{self, Command};

pub(crate) fn upgrade_packages() {
    let version = env!("CARGO_PKG_VERSION");
    let toml_path = std::path::Path::new("Cargo.toml");
    if !toml_path.exists() {
        eprintln!("Error: no Cargo.toml found in the current directory");
        process::exit(1);
    }
    let content = fs::read_to_string(toml_path).unwrap_or_else(|e| {
        eprintln!("Failed to read Cargo.toml: {}", e);
        process::exit(1);
    });
    let (rewritten, changed) = upgrade_cargo_toml(&content, version);
    if changed == 0 {
        println!("No ecat-* dependencies found in Cargo.toml");
        return;
    }
    fs::write(toml_path, rewritten).unwrap_or_else(|e| {
        eprintln!("Failed to write Cargo.toml: {}", e);
        process::exit(1);
    });
    println!(
        "Updated {} ecat-* dependency requirement(s) to {}",
        changed, version
    );
    let status = Command::new("cargo")
        .arg("update")
        .status()
        .unwrap_or_else(|e| {
            eprintln!("Failed to run cargo update: {}", e);
            process::exit(1);
        });
    if !status.success() {
        eprintln!("cargo update failed; Cargo.lock may be out of date");
        process::exit(status.code().unwrap_or(1));
    }
    println!("Cargo.lock updated");
}

/// Rewrite ecat/ecat-* version requirements in dependency tables.
fn upgrade_cargo_toml(content: &str, version: &str) -> (String, usize) {
    let mut in_deps = false;
    let mut changed = 0;
    let mut out: Vec<String> = Vec::with_capacity(content.lines().count());
    for line in content.lines() {
        let t = line.trim();
        if t.starts_with('[') && t.ends_with(']') {
            in_deps = matches!(
                t,
                "[dependencies]"
                    | "[workspace.dependencies]"
                    | "[dev-dependencies]"
                    | "[build-dependencies]"
            );
            out.push(line.to_string());
            continue;
        }
        if in_deps && let Some(rewritten) = rewrite_ecat_line(line, version) {
            changed += 1;
            out.push(rewritten);
            continue;
        }
        out.push(line.to_string());
    }
    (out.join("\n") + "\n", changed)
}

fn rewrite_ecat_line(line: &str, version: &str) -> Option<String> {
    let eq = line.find('=')?;
    let key = line[..eq].trim();
    if key != "ecat" && !key.starts_with("ecat-") {
        return None;
    }
    let rest = &line[eq + 1..];
    let rest_trimmed = rest.trim();
    if rest_trimmed.starts_with('"') {
        // Plain string requirement: ecat = "1.0"
        let start = line.find('"')?;
        let end = line.rfind('"')?;
        if end <= start || &line[start + 1..end] == version {
            return None;
        }
        let mut s = line.to_string();
        s.replace_range(start + 1..end, version);
        Some(s)
    } else if rest_trimmed.starts_with('{') {
        // Inline table: ecat = { path = "..", version = "1.0" }
        let marker = "version = \"";
        let rel = rest.find(marker)?;
        let val_start = rel + marker.len();
        let rem = &rest[val_start..];
        let val_len = rem.find('"')?;
        let abs_start = eq + 1 + val_start;
        if &line[abs_start..abs_start + val_len] == version {
            return None;
        }
        let mut s = line.to_string();
        s.replace_range(abs_start..abs_start + val_len, version);
        Some(s)
    } else {
        None
    }
}

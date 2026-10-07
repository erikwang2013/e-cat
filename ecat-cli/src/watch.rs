// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use std::process::{self, Command};
use std::sync::mpsc;
use std::time::Duration;

pub(crate) fn run_cargo_run() {
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

pub(crate) fn run_watch() {
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
    let mut child = spawn_cargo_run();
    loop {
        if rx.recv().is_err() {
            break;
        }
        // debounce: only restart after 500ms of silence
        while rx.recv_timeout(Duration::from_millis(500)).is_ok() {}
        println!("\nChange detected, restarting...");
        stop_child(&mut child);
        child = spawn_cargo_run();
    }
}

fn spawn_cargo_run() -> std::process::Child {
    let mut cmd = Command::new("cargo");
    cmd.arg("run");
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd.spawn().unwrap_or_else(|e| {
        eprintln!("Failed to start: {}", e);
        process::exit(1);
    })
}

/// Stop the `cargo run` child and, on unix, the whole process group it leads,
/// so the spawned service binary does not survive as an orphan holding the port.
fn stop_child(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        let pid = child.id() as i32;
        // ESRCH is fine: the child already exited and took its group with it.
        unsafe {
            libc::kill(-pid, libc::SIGTERM);
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

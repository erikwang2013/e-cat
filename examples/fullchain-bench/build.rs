// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
fn main() {
    println!("cargo:rerun-if-changed=proto/bench.proto");
    tonic_build::configure()
        .compile_protos(&["proto/bench.proto"], &["proto"])
        .unwrap();
}

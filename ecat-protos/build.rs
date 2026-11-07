// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
fn main() {
    tonic_prost_build::configure()
        .compile_protos(&["proto/errors.proto", "proto/metadata.proto"], &["proto"])
        .unwrap();
}

// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
pub mod errors {
    tonic::include_proto!("ecat.errors");
}

pub mod metadata {
    tonic::include_proto!("ecat.metadata");
}

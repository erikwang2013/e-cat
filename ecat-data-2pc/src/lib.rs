// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 显式 opt-in 的 XA 2PC 协调器；不接入 `KeyedClient` / `scatter`。
mod coordinator;
mod recovery;
mod xa;

pub use coordinator::{DistributedTx, RecoveryReport, TwoPhaseCoordinator, XaError, XaParticipant};
#[cfg(feature = "sqlx")]
pub use xa::SqlxXaParticipant;

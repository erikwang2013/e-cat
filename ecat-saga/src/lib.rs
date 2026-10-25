// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 轻量 Saga 引擎：顺序执行、逆序补偿、持久化续跑。
mod saga;
mod state;

pub use saga::{Saga, SagaContext, SagaError, SagaStep};
pub use state::{
    MemorySagaStateStore, SagaState, SagaStateStore, SagaStatus, SqlSagaStateStore, StepStatus,
};

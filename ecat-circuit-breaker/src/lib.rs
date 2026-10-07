// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 熔断中间件。
//!
//! - `CircuitBreakerLayer` / `CircuitBreakerService`：tower 集成，逐请求包裹下游服务。
//! - `Breaker` + `BreakerConfig`：不依赖 tower 的熔断状态机，供非 tower 场景
//!   （如 RDBMS 端点路由）逐端点持有，`state()` 可用于跳过已熔断端点。

mod breaker;
mod tower;
mod window;

pub use breaker::{Breaker, BreakerConfig, BreakerError, BreakerState};
pub use tower::{CircuitBreakerLayer, CircuitBreakerService};

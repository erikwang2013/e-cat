// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! Saga 执行：顺序执行步骤，失败逆序补偿；状态每步落库，可 resume。
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::state::{SagaState, SagaStateStore, SagaStatus, StepStatus};

#[async_trait]
pub trait SagaStep: Send + Sync {
    fn name(&self) -> &str;
    async fn execute(&self, ctx: &mut SagaContext) -> Result<(), SagaError>;
    async fn compensate(&self, ctx: &mut SagaContext) -> Result<(), SagaError>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SagaContext {
    pub saga_id: String,
    pub idempotency_key: String,
    pub data: Value,
}

#[derive(Debug, thiserror::Error)]
pub enum SagaError {
    #[error("saga step `{step}` failed: {message}")]
    StepFailed { step: String, message: String },
    #[error("saga compensation `{step}` failed: {message}")]
    CompensationFailed { step: String, message: String },
    #[error("saga `{saga_id}` not found")]
    NotFound { saga_id: String },
    #[error("saga `{saga_id}` is {status:?}; manual intervention required")]
    InvalidState { saga_id: String, status: SagaStatus },
    #[error("saga state: {0}")]
    State(String),
    #[error("saga context: {0}")]
    Context(String),
}

/// 步骤 + 状态存储。步骤和补偿都必须是幂等的。
pub struct Saga {
    steps: Vec<Arc<dyn SagaStep>>,
    store: Arc<dyn SagaStateStore>,
}

impl Saga {
    pub fn new(steps: Vec<Arc<dyn SagaStep>>, store: Arc<dyn SagaStateStore>) -> Self {
        Self { steps, store }
    }

    /// 首次运行；已处于终态时幂等返回 Ok，未完成则从持久化状态续跑。
    pub async fn run(&self, saga_id: &str, mut ctx: SagaContext) -> Result<(), SagaError> {
        ctx.saga_id = saga_id.to_owned();
        if let Some(state) = self.store.load(saga_id).await? {
            return self.continue_state(state).await;
        }
        let state = SagaState {
            saga_id: saga_id.to_owned(),
            status: SagaStatus::Running,
            current_step: 0,
            context: encode_context(&ctx)?,
        };
        self.store.save(&state).await?;
        self.forward(state, ctx).await
    }

    /// 从持久化状态续跑；终态直接返回，`Failed` 需要人工处理。
    pub async fn resume(&self, saga_id: &str) -> Result<(), SagaError> {
        let state = self
            .store
            .load(saga_id)
            .await?
            .ok_or_else(|| SagaError::NotFound {
                saga_id: saga_id.to_owned(),
            })?;
        self.continue_state(state).await
    }

    async fn continue_state(&self, state: SagaState) -> Result<(), SagaError> {
        if state.current_step > self.steps.len() {
            return Err(SagaError::State(format!(
                "saga `{}` current_step {} exceeds {} steps",
                state.saga_id,
                state.current_step,
                self.steps.len()
            )));
        }
        match state.status {
            SagaStatus::Completed | SagaStatus::Compensated => Ok(()),
            SagaStatus::Failed => Err(SagaError::InvalidState {
                saga_id: state.saga_id,
                status: state.status,
            }),
            SagaStatus::Compensating => {
                let ctx = decode_context(&state)?;
                let before = state.current_step;
                self.compensate(state, ctx, before, None).await
            }
            SagaStatus::Running => {
                let ctx = decode_context(&state)?;
                self.forward(state, ctx).await
            }
        }
    }

    async fn forward(&self, mut state: SagaState, mut ctx: SagaContext) -> Result<(), SagaError> {
        for index in state.current_step..self.steps.len() {
            let step = &self.steps[index];
            if self.store.step_status(&state.saga_id, step.name()).await? == StepStatus::Done {
                continue;
            }
            match step.execute(&mut ctx).await {
                Ok(()) => {
                    self.store
                        .set_step(&state.saga_id, step.name(), StepStatus::Done)
                        .await?;
                    state.current_step = index + 1;
                    state.context = encode_context(&ctx)?;
                    self.store.save(&state).await?;
                }
                Err(error) => {
                    self.store
                        .set_step(&state.saga_id, step.name(), StepStatus::Failed)
                        .await?;
                    state.status = SagaStatus::Compensating;
                    state.context = encode_context(&ctx)?;
                    self.store.save(&state).await?;
                    let cause = SagaError::StepFailed {
                        step: step.name().to_owned(),
                        message: error.to_string(),
                    };
                    return self.compensate(state, ctx, index, Some(cause)).await;
                }
            }
        }
        state.status = SagaStatus::Completed;
        state.context = encode_context(&ctx)?;
        self.store.save(&state).await?;
        Ok(())
    }

    /// 逆序补偿 `0..before` 中状态为 Done 的步骤；补偿失败即停，不重试。
    async fn compensate(
        &self,
        mut state: SagaState,
        mut ctx: SagaContext,
        before: usize,
        cause: Option<SagaError>,
    ) -> Result<(), SagaError> {
        for index in (0..before.min(self.steps.len())).rev() {
            let step = &self.steps[index];
            if self.store.step_status(&state.saga_id, step.name()).await? != StepStatus::Done {
                continue;
            }
            if let Err(error) = step.compensate(&mut ctx).await {
                self.store
                    .set_step(&state.saga_id, step.name(), StepStatus::Failed)
                    .await?;
                state.status = SagaStatus::Failed;
                state.context = encode_context(&ctx)?;
                self.store.save(&state).await?;
                tracing::error!(
                    saga_id = %state.saga_id,
                    step = step.name(),
                    error = %error,
                    "saga compensation failed"
                );
                return Err(SagaError::CompensationFailed {
                    step: step.name().to_owned(),
                    message: error.to_string(),
                });
            }
            self.store
                .set_step(&state.saga_id, step.name(), StepStatus::Compensated)
                .await?;
            state.context = encode_context(&ctx)?;
            self.store.save(&state).await?;
        }
        state.status = SagaStatus::Compensated;
        state.context = encode_context(&ctx)?;
        self.store.save(&state).await?;
        match cause {
            Some(cause) => Err(cause),
            None => Ok(()),
        }
    }
}

fn encode_context(ctx: &SagaContext) -> Result<Value, SagaError> {
    serde_json::to_value(ctx)
        .map_err(|e| SagaError::Context(format!("failed to encode saga context: {e}")))
}

fn decode_context(state: &SagaState) -> Result<SagaContext, SagaError> {
    serde_json::from_value(state.context.clone())
        .map_err(|e| SagaError::Context(format!("failed to decode saga context: {e}")))
}

#[cfg(test)]
mod tests;

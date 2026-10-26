// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 与 HTTP 端点等价的 gRPC service。
use std::time::Instant;

use tonic::{Request, Response, Status};

use crate::pb::bench_server::{Bench, BenchServer};
use crate::pb::{Key, PingReply, PingRequest, Value, WriteReply, WriteRequest};
use crate::state::{self, BenchState, ROWS};

#[derive(Clone)]
pub struct BenchService {
    state: BenchState,
}

impl BenchService {
    pub fn new(state: BenchState) -> Self {
        Self { state }
    }

    pub fn server(self) -> BenchServer<Self> {
        BenchServer::new(self)
    }
}

#[tonic::async_trait]
impl Bench for BenchService {
    async fn ping(&self, _request: Request<PingRequest>) -> Result<Response<PingReply>, Status> {
        let started = Instant::now();
        Ok(Response::new(PingReply {
            server_us: started.elapsed().as_micros() as u64,
        }))
    }

    async fn read(&self, request: Request<Key>) -> Result<Response<Value>, Status> {
        let started = Instant::now();
        let id = request.into_inner().id.rem_euclid(ROWS);
        let (value, cache_len) = state::read(&self.state, id)
            .await
            .map_err(Status::internal)?;
        Ok(Response::new(Value {
            value,
            cache_len: cache_len as u64,
            server_us: started.elapsed().as_micros() as u64,
        }))
    }

    async fn write(&self, request: Request<WriteRequest>) -> Result<Response<WriteReply>, Status> {
        let started = Instant::now();
        let request = request.into_inner();
        let id = request.id.rem_euclid(ROWS);
        state::write(&self.state, id, &request.value)
            .await
            .map_err(Status::internal)?;
        Ok(Response::new(WriteReply {
            ok: true,
            server_us: started.elapsed().as_micros() as u64,
        }))
    }
}

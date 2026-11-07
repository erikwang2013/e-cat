# e-cat example Dockerfile (multi-stage)
# NOTE: This image runs the helloworld example service. For production,
# replace the helloworld build/copy below with your own service binary.
# edition 2024 needs >= 1.85; tonic 0.14 / lapin 4 declare rust-version 1.88.
FROM rust:1.88-slim AS builder
# ecat-mq-rabbitmq / ecat-mq-mqtt 走 native-tls（OpenSSL），构建期需要头文件；
# ecat-protos 的 tonic-prost-build 需要 protoc。
RUN apt-get update && apt-get install -y pkg-config libssl-dev protobuf-compiler && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY . .
RUN cargo build --release -p ecat-cli -p helloworld

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates curl && rm -rf /var/lib/apt/lists/*
# The CLI binary is named `ecat` (from ecat-cli), not `ecat-app`.
COPY --from=builder /app/target/release/ecat /usr/local/bin/ecat
# Example service binary; swap for your own service in production.
COPY --from=builder /app/target/release/helloworld /usr/local/bin/helloworld
EXPOSE 8000 9000
HEALTHCHECK --interval=10s --timeout=3s CMD curl -f http://localhost:8000/health || exit 1
CMD ["/usr/local/bin/helloworld"]

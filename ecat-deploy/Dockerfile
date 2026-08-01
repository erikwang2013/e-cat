# e-cat production Dockerfile (multi-stage)
FROM rust:1.80-slim AS builder
WORKDIR /app
COPY . .
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/ecat-app /usr/local/bin/ecat-app
EXPOSE 8000 9000
HEALTHCHECK --interval=10s --timeout=3s CMD curl -f http://localhost:8000/health || exit 1
CMD ["ecat-app"]

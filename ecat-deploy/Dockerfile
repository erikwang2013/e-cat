# e-cat production Dockerfile (multi-stage)
# The workspace builds with edition 2024, so the builder image must be >= 1.85.
FROM rust:1.85-slim AS builder
WORKDIR /app
COPY . .
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates curl && rm -rf /var/lib/apt/lists/*
# The CLI binary is named `ecat` (from ecat-cli), not `ecat-app`.
COPY --from=builder /app/target/release/ecat /usr/local/bin/ecat
EXPOSE 8000 9000
HEALTHCHECK --interval=10s --timeout=3s CMD curl -f http://localhost:8000/health || exit 1
CMD ["ecat"]

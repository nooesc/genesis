# Multi-stage build for Genesis
FROM rust:1.86-slim AS builder

WORKDIR /usr/src/genesis
COPY . .

RUN apt-get update && apt-get install -y pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*
RUN cargo build --release --quiet

# Runtime image
FROM debian:bookworm-slim

RUN apt-get update && \
    apt-get install -y ca-certificates && \
    rm -rf /var/lib/apt/lists/*

COPY --from=builder /usr/src/genesis/target/release/genesis /usr/local/bin/genesis

# Create data directory
RUN mkdir -p /data/genesis

ENV GENESIS_DATA_DIR=/data/genesis
ENV GENESIS_DATABASE_PATH=/data/genesis/genesis.db

EXPOSE 3000

# Default: run the gateway server
CMD ["genesis", "serve", "--host", "0.0.0.0", "--port", "3000"]

# Multi-stage Dockerfile for Charcoal on Railway.
#
# Stage 1: Build (Rust + Node.js on Ubuntu 24.04 for glibc 2.39)
# Stage 2: Runtime (slim image with just the binary)
#
# Ubuntu 24.04 provides glibc 2.39 which satisfies ort-sys's requirement
# for __isoc23_strtoll (glibc 2.38+).

# ── Build stage ──────────────────────────────────────────────────────
FROM ubuntu:24.04 AS builder

ENV DEBIAN_FRONTEND=noninteractive

# Install build dependencies
RUN apt-get update && apt-get install -y --no-install-recommends \
    curl \
    build-essential \
    pkg-config \
    libssl-dev \
    ca-certificates \
    git \
    nodejs \
    npm \
    && rm -rf /var/lib/apt/lists/*

# Install Rust
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain 1.94.0
ENV PATH="/root/.cargo/bin:${PATH}"

WORKDIR /app

# Copy dependency manifests first for layer caching
COPY Cargo.toml Cargo.lock ./
COPY web/package.json web/package-lock.json ./web/

# Create a dummy src so cargo can resolve dependencies
RUN mkdir src && echo 'fn main() {}' > src/main.rs && \
    mkdir -p src/web && touch src/lib.rs

# Pre-fetch Cargo dependencies (cached unless Cargo.toml/lock changes)
RUN cargo fetch --locked

# Install Node dependencies (cached unless package.json/lock changes)
RUN cd web && npm ci

# Copy the full source
COPY . .

# Build SvelteKit SPA first (include_dir! embeds it at compile time)
RUN cd web && npm run build

# Build the release binary
RUN cargo build --release --features web,postgres

# ── Runtime stage ────────────────────────────────────────────────────
FROM ubuntu:24.04

ENV DEBIAN_FRONTEND=noninteractive

# Runtime dependencies only
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    libssl3t64 \
    libstdc++6 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy the built binary
COPY --from=builder /app/target/release/charcoal /app/charcoal

# Railway sets PORT at runtime
ENV RUST_LOG=charcoal=info

EXPOSE 3000

CMD ["./charcoal", "serve", "--port", "3000", "--bind", "0.0.0.0"]

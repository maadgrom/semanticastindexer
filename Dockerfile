# syntax=docker/dockerfile:1
#
# semanticastindexer CI/CD images. Two bases, because musl (Alpine) can't run the ort
# ONNX Runtime (no musl build):
#
#   alpine      (DEFAULT, :latest)      Alpine/musl, lean: qdrant + duckdb + ollama + ast + mcp.
#                                       No local ONNX embedder. Tiny; for Qdrant/Ollama CI.
#   full        (:*-full)               debian/glibc, --features all (adds the ort embedder).
#
# The ort embedder downloads the model + tokenizer on first run (cache it across runs by
# mounting a volume at HF_HOME). `sync` shells out to git, so every runtime ships git.

# =========================================================================================
# Alpine / musl  (lean: no ort)
# =========================================================================================
FROM rust:1-alpine AS builder-alpine
# Cap parallel compile jobs to bound peak RAM (DuckDB's bundled C++ is memory-heavy).
# Override with --build-arg CARGO_BUILD_JOBS=N; default 4 ~ a standard CI runner.
ARG CARGO_BUILD_JOBS=4
ENV CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS}
RUN apk add --no-cache build-base cmake pkgconf perl linux-headers \
        openssl-dev openssl-libs-static git
# Statically link OpenSSL (native-tls) into the musl binary.
ENV OPENSSL_STATIC=1 OPENSSL_NO_VENDOR=1 OPENSSL_DIR=/usr
WORKDIR /src
# Compile the dependency graph (dominated by DuckDB's bundled C++) against a stub
# main.rs first: this layer is keyed on Cargo.toml/Cargo.lock alone, so the CI layer
# cache reuses it across source-only changes instead of rebuilding every crate.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs \
    && cargo build --release --features "qdrant,duckdb,ollama,ast,mcp" \
    && rm -rf src target/release/semanticastindexer* \
              target/release/deps/semanticastindexer* \
              target/release/.fingerprint/semanticastindexer*
COPY . .
RUN cargo build --release --features "qdrant,duckdb,ollama,ast,mcp"

FROM alpine:3 AS alpine
LABEL org.opencontainers.image.source="https://github.com/maadgrom/semanticastindexer"
LABEL org.opencontainers.image.description="Semantic code search and near-duplicate detection (lean, musl)"
LABEL org.opencontainers.image.licenses="MIT"
RUN apk add --no-cache git ca-certificates
COPY --from=builder-alpine /src/target/release/semanticastindexer /usr/local/bin/semanticastindexer
WORKDIR /repo
ENTRYPOINT ["semanticastindexer"]
CMD ["--help"]

# =========================================================================================
# glibc / debian  (full: --features all, includes the ort ONNX embedder)
# =========================================================================================
# trixie (not bookworm): ort 2.0.0-rc.12's prebuilt ONNX Runtime needs GCC 13+ libstdc++
# symbols (__cxa_call_terminate); bookworm's GCC 12 toolchain fails at link.
FROM rust:1-trixie AS builder-full
# Cap parallel compile jobs to bound peak RAM (override with --build-arg CARGO_BUILD_JOBS=N).
ARG CARGO_BUILD_JOBS=4
ENV CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS}
RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential cmake pkg-config libssl-dev ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
# Same stub-build dependency layer as the Alpine builder (see comment there).
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs \
    && cargo build --release --features all \
    && rm -rf src target/release/semanticastindexer* \
              target/release/deps/semanticastindexer* \
              target/release/.fingerprint/semanticastindexer*
COPY . .
RUN cargo build --release --features all
# Carry the ONNX Runtime shared lib(s) ort downloaded (empty/harmless if it static-links).
RUN mkdir -p /onnxlibs \
    && find / -name 'libonnxruntime*.so*' -exec cp -av {} /onnxlibs/ \; 2>/dev/null || true

FROM debian:trixie-slim AS full
LABEL org.opencontainers.image.source="https://github.com/maadgrom/semanticastindexer"
LABEL org.opencontainers.image.description="Semantic code search and near-duplicate detection (full, glibc + ort)"
LABEL org.opencontainers.image.licenses="MIT"
RUN apt-get update && apt-get install -y --no-install-recommends git ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder-full /src/target/release/semanticastindexer /usr/local/bin/semanticastindexer
COPY --from=builder-full /onnxlibs/ /usr/local/lib/
ENV LD_LIBRARY_PATH=/usr/local/lib
WORKDIR /repo
ENTRYPOINT ["semanticastindexer"]
CMD ["--help"]

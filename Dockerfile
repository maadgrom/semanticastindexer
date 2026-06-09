# syntax=docker/dockerfile:1
#
# semanticastindexer CI/CD images. Two bases, because musl (Alpine) can't run the ort
# ONNX Runtime (no musl build):
#
#   alpine      (DEFAULT, :latest)      Alpine/musl, lean: qdrant + duckdb + ollama + ast + mcp.
#                                       No local ONNX embedder. Tiny; for Qdrant/Ollama CI.
#   full        (:*-full)               debian/glibc, --features all (adds the ort embedder).
#   with-model  (:*-with-model)         full + the default ONNX model baked into the HF cache.
#
# `sync` shells out to git, so every runtime ships git.

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
FROM rust:1-bookworm AS builder-full
# Cap parallel compile jobs to bound peak RAM (override with --build-arg CARGO_BUILD_JOBS=N).
ARG CARGO_BUILD_JOBS=4
ENV CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS}
RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential cmake pkg-config libssl-dev ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
RUN cargo build --release --features all
# Carry the ONNX Runtime shared lib(s) ort downloaded (empty/harmless if it static-links).
RUN mkdir -p /onnxlibs \
    && find / -name 'libonnxruntime*.so*' -exec cp -av {} /onnxlibs/ \; 2>/dev/null || true

FROM debian:bookworm-slim AS full
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

# =========================================================================================
# with-model  (full + the default ONNX model baked in for a network-free first run)
# =========================================================================================
# The pinned hf-hub (0.3) cannot fetch this repo's tokenizer.json from HF Xet storage, so we
# stage it with curl between two index passes (the onnx/model.onnx download works on its own).
FROM full AS with-model
RUN apt-get update && apt-get install -y --no-install-recommends curl \
    && rm -rf /var/lib/apt/lists/*
ENV HF_HOME=/opt/hf-cache
RUN mkdir -p /tmp/seed/src \
    && printf 'export function seed(): number { return 1 }\n' > /tmp/seed/src/seed.ts \
    && cd /tmp/seed \
    && (semanticastindexer --root src --ext ts --backend duckdb --embedder ort --collection seed --silent || true) \
    && SNAP=$(ls -d /opt/hf-cache/hub/models--jinaai--jina-embeddings-v2-base-code/snapshots/*/ 2>/dev/null | head -1) \
    && if [ -n "$SNAP" ] && [ ! -f "${SNAP}tokenizer.json" ]; then \
         curl -fsSL https://huggingface.co/jinaai/jina-embeddings-v2-base-code/resolve/main/tokenizer.json -o "${SNAP}tokenizer.json"; \
       fi \
    && semanticastindexer --root src --ext ts --backend duckdb --embedder ort --collection seed --silent \
    && rm -rf /tmp/seed
WORKDIR /repo
ENTRYPOINT ["semanticastindexer"]
CMD ["--help"]

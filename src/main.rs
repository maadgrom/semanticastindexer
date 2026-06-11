//! Binary entrypoint: parse CLI args and dispatch into the library's [`app`] layer.
//!
//! The default backend is Qdrant Cloud with **server-side inference** (using
//! `intfloat/multilingual-e5-small` by default). When using the `ort` embedder
//! (the default for the local DuckDB backend), the recommended code-trained
//! model `jinaai/jina-embeddings-v2-base-code` is used instead.
//!
//! Connection is read from the environment (never hard-code the token):
//!   QDRANT_URL      e.g. https://<cluster-id>.<region>.aws.cloud.qdrant.io:6334
//!   QDRANT_API_KEY  cluster API key
//!
//! Usage (after `cargo build --release --features all`):
//!   # See exactly what would be indexed/skipped — no network, no upload:
//!   ./target/release/semanticastindexer --root src --dry-run
//!   # TS index (the language label on each chunk is derived per-file from its
//!   # extension — `.ts` → "ts", `.tsx` → "tsx"):
//!   ./target/release/semanticastindexer --root src --ext ts,tsx
//!   # Search only:
//!   ./target/release/semanticastindexer \
//!       --query-only --query "where do we create the qdrant collection"

use anyhow::Result;
use clap::Parser;

use semanticastindexer::{app, cli::Args};

#[tokio::main]
async fn main() -> Result<()> {
    app::run(Args::parse()).await
}

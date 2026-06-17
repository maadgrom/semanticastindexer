//! Repository layer: the [`VectorStore`] port + its concrete adapters over the existing,
//! unchanged backends. The sole storage path — the services dispatch over `Arc<dyn
//! VectorStore>`.
//!
//! The trait definition is unconditional; each adapter is feature-gated to the backend it
//! wraps (`qdrant` / `duckdb` / `#[cfg(test)]` mock), so every feature combination still
//! builds.

pub mod vectorstore;

// The `impl_vectorstore_delegate!` macro: the trivial newtype adapters (qdrant, mock) share
// one generated forwarding impl instead of hand-copied delegations. Only compiled where it is
// actually invoked — the qdrant adapter (`feature = "qdrant"`) and the `#[cfg(test)]` mock —
// so a backend set that uses neither (e.g. duckdb/ollama only) doesn't carry an unused macro.
#[cfg(any(feature = "qdrant", test))]
mod delegate;

pub use vectorstore::VectorStore;

#[cfg(feature = "duckdb")]
pub mod duckdb;
// `pub(crate)` under `#[cfg(test)]` so the service-layer unit tests (US-003) can construct
// a `MockStore` (and seed it via `MockBackend::with_rows`) for in-process, no-network,
// no-thread service tests. Still test-only — never ships.
#[cfg(test)]
pub(crate) mod mock;
#[cfg(feature = "qdrant")]
pub mod qdrant;

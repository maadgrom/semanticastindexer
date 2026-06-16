//! Repository layer: the [`VectorStore`] port + its concrete adapters over the existing,
//! unchanged backends. The sole storage path — the services dispatch over `Arc<dyn
//! VectorStore>`.
//!
//! The trait definition is unconditional; each adapter is feature-gated to the backend it
//! wraps (`qdrant` / `duckdb` / `#[cfg(test)]` mock), so every feature combination still
//! builds.

pub mod vectorstore;

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

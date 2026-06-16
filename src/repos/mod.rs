//! Repository layer: the [`VectorStore`] port + its concrete adapters over the existing,
//! unchanged backends. ADDITIVE — nothing consumes this yet (the services wire it in a
//! later story); the old enum-dispatched `crate::vectordbs::Backend` + `crate::worker`
//! path stays the active one.
//!
//! The trait definition is unconditional; each adapter is feature-gated to the backend it
//! wraps (`qdrant` / `duckdb` / `#[cfg(test)]` mock), so every feature combination still
//! builds (the port is dispatched as `Arc<dyn VectorStore>`).

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

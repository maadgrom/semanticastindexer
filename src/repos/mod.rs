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
#[cfg(test)]
mod mock;
#[cfg(feature = "qdrant")]
pub mod qdrant;

//! Domain layer: entities, value objects, and the resolved [`Plan`]. Depends only on
//! `std` + external crates (globset, anyhow, serde, …) — never on the infrastructure,
//! service, app, or transport layers. The innermost ring of the clean architecture.

pub mod chunk;
pub mod duplicate;
pub mod embedding;
pub mod hit;
pub mod plan;
pub mod report;
pub mod similar;

pub use chunk::CodeChunk;
pub use duplicate::{DupCluster, DupMember};
pub use embedding::{
    PASSAGE_PREFIX, PrefixStyle, QUERY_PREFIX, QWEN_QUERY_INSTRUCT, format_passage, format_query,
};
pub use hit::Hit;
pub use plan::Plan;
pub use report::{RefreshReport, ReindexOutcome};
pub use similar::SimilarTarget;

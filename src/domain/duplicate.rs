//! Near-duplicate cluster result shapes: [`DupCluster`] and its [`DupMember`]s.

use serde::Serialize;

/// One member of a near-duplicate cluster.
#[derive(Debug, Clone, Serialize)]
pub struct DupMember {
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub symbol: Option<String>,
}

/// A near-duplicate cluster: its members plus the min/max edge similarity within it.
#[derive(Debug, Clone, Serialize)]
pub struct DupCluster {
    pub size: usize,
    pub members: Vec<DupMember>,
    pub min_sim: f32,
    pub max_sim: f32,
}

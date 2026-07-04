pub mod cypher;
pub mod docs;
pub mod embeddings;
pub mod graph;
pub mod ingest;
pub mod language;
pub mod obsidian;
pub mod project;
pub mod queries;

pub use cypher::run_query as run_cypher_query;
pub use graph::{CodeSearchHit, Direction, EdgeKind, GraphStore, NodeKind, NodeRecord};
pub use ingest::{index_directory, IndexStats};
pub use language::Language;
pub use obsidian::export_obsidian;
pub use project::{
    artifact_path, delete_project, export_project, graph_db_path, import_project, index_project,
    project_disk_usage, touch_queried,
};
pub use queries::{
    call_graph_dot, detect_changes, detect_dead_code, get_architecture, get_file_context,
    open_store, plan_query, search_code, semantic_search, ArchitectureSummary, QueryPlanResult,
    SemanticHit,
};

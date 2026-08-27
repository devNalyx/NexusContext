pub mod cypher;
pub mod docs;
pub mod enrich;
pub mod graph;
pub mod ingest;
pub mod language;
pub mod lsp;
pub mod obsidian;
pub mod project;
pub mod queries;
pub mod secure_fs;

pub use cypher::run_query as run_cypher_query;
pub use enrich::{enrich_with_lsp, EnrichmentReport};
pub use graph::{CodeSearchHit, Direction, EdgeKind, GraphStore, NodeKind, NodeRecord, TracedNode};
pub use ingest::{
    content_signature, estimate_watch_count, index_directory, index_directory_checked, IndexStats,
};
pub use language::Language;
pub use obsidian::export_obsidian;
pub use project::{
    artifact_path, delete_project, export_project, graph_db_path, import_project, index_project,
    index_project_deep, indexing_status, note_possible_supersession, project_disk_usage,
    record_auto_reindex, touch_and_catchup, touch_queried, IndexingStatus,
};
pub use queries::{
    call_graph_dot, detect_changes, detect_changes_blast_radius, detect_dead_code,
    get_architecture, get_file_context, open_store, plan_query, search_code, ArchitectureSummary,
    BlastRadiusResult, QueryPlanResult,
};

pub mod db;
pub mod error;
pub mod ops;
pub mod prime;
pub mod protocol;
pub mod search;
pub mod snapshot;
pub mod types;

pub use db::CairnDb;
pub use error::{CairnError, Result};
pub use types::*;

use std::path::Path;

/// The main facade for all Cairn operations.
///
/// Wraps `CairnDb` and provides methods for every operation
/// so CLI and MCP consumers have a single entry point.
pub struct Cairn {
    db: CairnDb,
}

impl Cairn {
    /// Open a persistent graph at the given path.
    pub async fn open(path: &Path) -> Result<Self> {
        let db = CairnDb::open(path).await?;
        Ok(Self { db })
    }

    /// Open an in-memory graph (for tests).
    pub async fn open_memory() -> Result<Self> {
        let db = CairnDb::open_memory().await?;
        Ok(Self { db })
    }

    /// The database path (or ":memory:").
    pub fn db_path(&self) -> &str {
        &self.db.db_path
    }

    // ── Initialization ───────────────────────────────────────────

    /// Initialize default voice and preferences if they don't exist.
    pub async fn init_defaults(&self, initial_voice: Option<&str>) -> Result<()> {
        prime::init_defaults(&self.db, initial_voice).await
    }

    // ── Mutation operations (ops.rs) ─────────────────────────────

    pub async fn learn(&self, params: LearnParams) -> Result<LearnResult> {
        ops::learn(&self.db, params).await
    }

    pub async fn connect(&self, params: ConnectParams) -> Result<ConnectResult> {
        ops::connect(&self.db, params).await
    }

    pub async fn amend(&self, params: AmendParams) -> Result<AmendResult> {
        ops::amend(&self.db, params).await
    }

    pub async fn forget(&self, params: ForgetParams) -> Result<ForgetResult> {
        ops::forget(&self.db, params).await
    }

    pub async fn rewrite(&self, params: RewriteParams) -> Result<RewriteResult> {
        ops::rewrite(&self.db, params).await
    }

    pub async fn rename(&self, params: RenameParams) -> Result<RenameResult> {
        ops::rename(&self.db, params).await
    }

    pub async fn reset(&self) -> Result<()> {
        ops::reset(&self.db).await
    }

    pub async fn checkpoint(&self, params: CheckpointParams) -> Result<CheckpointResult> {
        ops::checkpoint(&self.db, params).await
    }

    pub async fn history(&self, params: HistoryParams) -> Result<HistoryResult> {
        ops::history(&self.db, params).await
    }

    // ── Query operations (search.rs) ─────────────────────────────

    pub async fn search(&self, params: SearchParams) -> Result<SearchResult> {
        search::search(&self.db, params).await
    }

    pub async fn explore(&self, params: ExploreParams) -> Result<ExploreResult> {
        search::explore(&self.db, params).await
    }

    pub async fn path(&self, params: PathParams) -> Result<PathResult> {
        search::path(&self.db, params).await
    }

    pub async fn nearby(&self, params: NearbyParams) -> Result<NearbyResult> {
        search::nearby(&self.db, params).await
    }

    pub async fn stats(&self) -> Result<StatsResult> {
        search::stats(&self.db).await
    }

    pub async fn graph_view(&self) -> Result<GraphViewResult> {
        search::graph_view(&self.db).await
    }

    // ── Context & status (prime.rs) ──────────────────────────────

    pub async fn prime(&self, params: PrimeParams) -> Result<PrimeResult> {
        prime::prime(&self.db, params).await
    }

    pub async fn graph_status(&self) -> Result<GraphStatusResult> {
        prime::graph_status(&self.db).await
    }

    // ── Voice & preferences ──────────────────────────────────────

    pub async fn get_voice(&self) -> Result<Option<Voice>> {
        prime::get_voice(&self.db).await
    }

    pub async fn set_voice(&self, content: &str) -> Result<VoiceResult> {
        prime::set_voice(&self.db, content).await
    }

    pub async fn get_preferences(&self) -> Result<Preferences> {
        prime::get_preferences(&self.db).await
    }

    pub async fn set_preferences(&self, prefs: &Preferences) -> Result<()> {
        prime::set_preferences(&self.db, prefs).await
    }

    // ── Snapshot & restore ───────────────────────────────────────

    pub async fn snapshot(&self, params: SnapshotParams) -> Result<SnapshotResult> {
        snapshot::snapshot(&self.db, params).await
    }

    pub async fn restore(&self, params: RestoreParams) -> Result<RestoreResult> {
        snapshot::restore(&self.db, params).await
    }

    pub async fn export_json(&self) -> Result<String> {
        snapshot::export_json(&self.db).await
    }

    pub async fn import_json(&self, json: &str) -> Result<(usize, usize)> {
        snapshot::import_json(&self.db, json).await
    }

    pub fn list_snapshots(&self) -> Result<Vec<SnapshotResult>> {
        snapshot::list_snapshots()
    }
}

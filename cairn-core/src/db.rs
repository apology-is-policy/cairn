use std::path::Path;

use surrealdb::engine::local::{Db, SurrealKv};
use surrealdb::Surreal;

use crate::error::{CairnError, Result};

const SCHEMA: &str = include_str!("schema.surql");

/// The schema version this binary knows how to handle.
/// Bump when adding migrations.
pub const CURRENT_SCHEMA_VERSION: i64 = 2;

pub struct CairnDb {
    pub(crate) db: Surreal<Db>,
    pub db_path: String,
}

impl CairnDb {
    /// Open a persistent database at the given path.
    pub async fn open(path: &Path) -> Result<Self> {
        let db = Surreal::new::<SurrealKv>(path)
            .await
            .map_err(|e| CairnError::Db(e.to_string()))?;

        let db_path = path.display().to_string();
        let cairn_db = Self { db, db_path };
        cairn_db.init().await?;
        Ok(cairn_db)
    }

    /// Open an in-memory database (for tests).
    pub async fn open_memory() -> Result<Self> {
        use surrealdb::engine::local::Mem;

        let db = Surreal::new::<Mem>(())
            .await
            .map_err(|e| CairnError::Db(e.to_string()))?;

        let cairn_db = Self {
            db,
            db_path: ":memory:".into(),
        };
        cairn_db.init().await?;
        Ok(cairn_db)
    }

    async fn init(&self) -> Result<()> {
        self.db
            .use_ns("cairn")
            .use_db("main")
            .await
            .map_err(|e| CairnError::Db(e.to_string()))?;

        self.run_migrations().await?;
        Ok(())
    }

    async fn run_migrations(&self) -> Result<()> {
        let db_version = self.get_schema_version().await?;

        if db_version > CURRENT_SCHEMA_VERSION {
            return Err(CairnError::SchemaVersionMismatch {
                db: db_version,
                binary: CURRENT_SCHEMA_VERSION,
            });
        }

        if db_version < 1 {
            self.db
                .query(SCHEMA)
                .await
                .map_err(|e| CairnError::Db(format!("Schema migration failed: {e}")))?;

            self.set_schema_version(1).await?;
        }

        if db_version < 2 {
            // v2: Add `locked` field to topic table.
            // DEFINE FIELD sets the default for new records, but existing
            // records keep NULL. Backfill explicitly.
            self.db
                .query(
                    "DEFINE FIELD locked ON topic TYPE bool DEFAULT false;
                     UPDATE topic SET locked = false WHERE locked = NONE;",
                )
                .await
                .map_err(|e| CairnError::Db(format!("v2 migration failed: {e}")))?;
            self.set_schema_version(2).await?;
        }

        Ok(())
    }

    /// Read the current schema version stored in the database.
    pub async fn schema_version(&self) -> Result<i64> {
        self.get_schema_version().await
    }

    async fn get_schema_version(&self) -> Result<i64> {
        // Try to read the schema version. If the table doesn't exist yet,
        // SurrealDB returns an empty result rather than an error.
        let mut res = self
            .db
            .query("SELECT version FROM meta:schema LIMIT 1")
            .await
            .map_err(|e| CairnError::Db(e.to_string()))?;

        let version: Option<i64> = res
            .take("version")
            .map_err(|e| CairnError::Db(e.to_string()))?;

        Ok(version.unwrap_or(0))
    }

    async fn set_schema_version(&self, version: i64) -> Result<()> {
        self.db
            .query("UPSERT meta:schema SET version = $version")
            .bind(("version", version))
            .await
            .map_err(|e| CairnError::Db(e.to_string()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_open_memory() {
        let db = CairnDb::open_memory().await.unwrap();
        assert_eq!(db.db_path, ":memory:");
    }

    #[tokio::test]
    async fn test_schema_version() {
        let db = CairnDb::open_memory().await.unwrap();
        let version = db.get_schema_version().await.unwrap();
        assert_eq!(version, CURRENT_SCHEMA_VERSION);
    }

    #[tokio::test]
    async fn test_tables_exist() {
        let db = CairnDb::open_memory().await.unwrap();

        // Verify the topic table is usable by querying it
        let mut res = db.db.query("SELECT * FROM topic LIMIT 1").await.unwrap();
        let topics: Vec<serde_json::Value> = res.take(0).unwrap();
        assert!(topics.is_empty());
    }

    #[tokio::test]
    async fn test_idempotent_migrations() {
        let db = CairnDb::open_memory().await.unwrap();
        // Running migrations again should be a no-op
        db.run_migrations().await.unwrap();
        let version = db.get_schema_version().await.unwrap();
        assert_eq!(version, CURRENT_SCHEMA_VERSION);
    }
}

mod collection;
mod embed;
mod record;
mod schema;
mod shard;
mod stats;

pub use collection::{
    Collection, CollectionInfo, CollectionInput, delete_collection, get_collection,
    list_collections, put_collection,
};
pub use embed::{EmbedResult, embed_records};
pub(crate) use record::rank_order;
pub use record::{
    DeleteResult, GetResult, ListResult, PutItem, PutResult, Record, delete_records, get_records,
    list_records, put_records,
};
pub use shard::{Shard, active_shard, ensure_active_shard, list_shards, refresh_shard};
pub use stats::{rebuild, stats};

use crate::db::Db;
use crate::embedding::Embedder;
use crate::error::{KvError, Result};
use crate::search::IndexCache;
use serde_json::Value;
use std::sync::{Arc, Mutex};

/// Store is backend-agnostic: FFI uses `FfiDb`, tests use an in-memory SQLite
/// adapter. All public methods call `ensure_schema` first.
pub struct Store {
    db: Arc<dyn Db>,
    embedder: Arc<dyn Embedder>,
    index: IndexCache,
    schema_ready: Mutex<bool>,
    prompt_seeded: Mutex<bool>,
}

impl Store {
    pub fn new(db: Arc<dyn Db>, embedder: Arc<dyn Embedder>) -> Self {
        Self {
            db,
            embedder,
            index: IndexCache::default(),
            schema_ready: Mutex::new(false),
            prompt_seeded: Mutex::new(false),
        }
    }

    pub fn embedder(&self) -> &dyn Embedder {
        self.embedder.as_ref()
    }

    pub fn index(&self) -> &IndexCache {
        &self.index
    }

    pub fn ensure_schema(&self) -> Result<()> {
        {
            let ready = self
                .schema_ready
                .lock()
                .map_err(|_| KvError::database("schema lock poisoned"))?;
            if *ready {
                return Ok(());
            }
        }

        schema::create_schema(self)?;
        schema::migrate_schema(self)?;

        {
            let mut ready = self
                .schema_ready
                .lock()
                .map_err(|_| KvError::database("schema lock poisoned"))?;
            *ready = true;
        }

        // Seed only after the flag is set: seeding uses the normal record API
        // and would otherwise recurse into ensure_schema.
        let seeded = self
            .prompt_seeded
            .lock()
            .map_err(|_| KvError::database("prompt lock poisoned"))?;
        if !*seeded {
            drop(seeded);
            crate::prompt::seed(self)?;
            let mut seeded = self
                .prompt_seeded
                .lock()
                .map_err(|_| KvError::database("prompt lock poisoned"))?;
            *seeded = true;
        }
        Ok(())
    }

    pub(crate) fn query(&self, name: &str, sql: &str) -> Result<Vec<Value>> {
        self.db
            .query(name, sql)
            .map_err(|err| KvError::database(format!("{err:#}")))
    }

    pub(crate) fn execute(&self, name: &str, sql: &str) -> Result<()> {
        self.db
            .execute(name, sql)
            .map_err(|err| KvError::database(format!("{err:#}")))
    }

    /// Direct database access for tests and maintenance helpers.
    pub fn db(&self) -> &dyn Db {
        self.db.as_ref()
    }
}

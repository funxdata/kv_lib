use crate::db::{as_i64, as_string, quote};
use crate::error::{KvError, Result};
use crate::store::Store;
use crate::store::collection::Collection;
use crate::store::schema::{META_DB, records_table_sql};

#[derive(Debug, Clone)]
pub struct Shard {
    pub id: i64,
    pub collection: String,
    pub shard_no: i64,
    pub path: String,
    pub row_count: i64,
    pub size_bytes: i64,
    pub state: i64,
}

impl Shard {
    fn from_row(row: &serde_json::Value) -> Self {
        Self {
            id: as_i64(row, "id"),
            collection: as_string(row, "collection"),
            shard_no: as_i64(row, "shard_no"),
            path: as_string(row, "path"),
            row_count: as_i64(row, "row_count"),
            size_bytes: as_i64(row, "size_bytes"),
            state: as_i64(row, "state"),
        }
    }
}

pub fn list_shards(store: &Store, collection: &str, include_broken: bool) -> Result<Vec<Shard>> {
    let filter = if include_broken {
        ""
    } else {
        " AND state != 2"
    };
    let rows = store.query(
        META_DB,
        &format!(
            "SELECT id, collection, shard_no, path, row_count, size_bytes, state
             FROM kv_shard WHERE collection = {}{} ORDER BY shard_no",
            quote(collection),
            filter
        ),
    )?;
    Ok(rows.iter().map(Shard::from_row).collect())
}

pub fn active_shard(store: &Store, collection: &str) -> Result<Option<Shard>> {
    let rows = store.query(
        META_DB,
        &format!(
            "SELECT id, collection, shard_no, path, row_count, size_bytes, state
             FROM kv_shard WHERE collection = {} AND state = 0
             ORDER BY shard_no DESC LIMIT 1",
            quote(collection)
        ),
    )?;
    Ok(rows.first().map(Shard::from_row))
}

pub fn ensure_active_shard(store: &Store, collection: &Collection) -> Result<Shard> {
    if let Some(shard) = active_shard(store, &collection.code)? {
        return Ok(shard);
    }

    let rows = store.query(
        META_DB,
        &format!(
            "SELECT COALESCE(MAX(shard_no), -1) AS n FROM kv_shard WHERE collection = {}",
            quote(&collection.code)
        ),
    )?;
    let next = rows.first().map(|row| as_i64(row, "n") + 1).unwrap_or(0);
    create_shard(store, collection, next)
}

fn create_shard(store: &Store, collection: &Collection, shard_no: i64) -> Result<Shard> {
    let path = format!("kv_{}_{}", collection.code, shard_no);
    store.execute(
        META_DB,
        &format!(
            "INSERT INTO kv_shard (collection, shard_no, path, row_count, size_bytes, state)
             VALUES ({}, {}, {}, 0, 0, 0)",
            quote(&collection.code),
            shard_no,
            quote(&path)
        ),
    )?;

    let created = (|| -> Result<()> {
        store.execute(&path, records_table_sql())?;
        store.execute(
            &path,
            "CREATE INDEX IF NOT EXISTS idx_kv_record_hash ON kv_record(text_hash)",
        )?;
        store.execute(
            &path,
            "CREATE INDEX IF NOT EXISTS idx_kv_record_ver ON kv_record(version)",
        )?;
        // PRAGMAs are best-effort: database.dll opens a fresh connection per call.
        let _ = store.query(&path, "PRAGMA journal_mode=WAL");
        let _ = store.query(&path, "PRAGMA synchronous=NORMAL");
        let _ = store.query(&path, "PRAGMA auto_vacuum=INCREMENTAL");
        Ok(())
    })();

    if let Err(err) = created {
        let _ = store.execute(
            META_DB,
            &format!(
                "DELETE FROM kv_shard WHERE collection = {} AND shard_no = {}",
                quote(&collection.code),
                shard_no
            ),
        );
        return Err(KvError::shard(format!(
            "failed to create shard {path}: {}",
            err.msg
        )));
    }

    let rows = store.query(
        META_DB,
        &format!(
            "SELECT id, collection, shard_no, path, row_count, size_bytes, state
             FROM kv_shard WHERE collection = {} AND shard_no = {} LIMIT 1",
            quote(&collection.code),
            shard_no
        ),
    )?;
    rows.first()
        .map(Shard::from_row)
        .ok_or_else(|| KvError::shard(format!("shard {path} was not registered")))
}

pub fn shard_size_bytes(store: &Store, shard: &Shard) -> i64 {
    let file = format!("./database/{}.db", shard.path);
    if let Ok(metadata) = std::fs::metadata(&file) {
        return metadata.len() as i64;
    }
    let page_count = store
        .query(&shard.path, "PRAGMA page_count")
        .ok()
        .and_then(|rows| rows.first().map(|row| as_i64(row, "page_count")))
        .unwrap_or(0);
    let page_size = store
        .query(&shard.path, "PRAGMA page_size")
        .ok()
        .and_then(|rows| rows.first().map(|row| as_i64(row, "page_size")))
        .unwrap_or(0);
    page_count.saturating_mul(page_size)
}

pub fn refresh_shard(store: &Store, shard: &Shard) -> Result<Shard> {
    let rows = store.query(&shard.path, "SELECT COUNT(*) AS n FROM kv_record")?;
    let row_count = rows.first().map(|row| as_i64(row, "n")).unwrap_or(0);
    let size_bytes = shard_size_bytes(store, shard);
    store.execute(
        META_DB,
        &format!(
            "UPDATE kv_shard SET row_count = {}, size_bytes = {}, updated_at = CURRENT_TIMESTAMP
             WHERE id = {}",
            row_count, size_bytes, shard.id
        ),
    )?;
    Ok(Shard {
        row_count,
        size_bytes,
        ..shard.clone()
    })
}

pub fn maybe_seal(store: &Store, collection: &Collection) -> Result<()> {
    if collection.shard_max_mb <= 0 {
        return Ok(());
    }
    let Some(active) = active_shard(store, &collection.code)? else {
        return Ok(());
    };
    let refreshed = refresh_shard(store, &active)?;
    let threshold = collection.shard_max_mb.saturating_mul(1024 * 1024);
    if refreshed.size_bytes < threshold {
        return Ok(());
    }

    store.execute(
        META_DB,
        &format!(
            "UPDATE kv_shard SET state = 1, updated_at = CURRENT_TIMESTAMP WHERE id = {}",
            refreshed.id
        ),
    )?;

    if let Err(err) = ensure_active_shard(store, collection) {
        // Roll back so the sealed shard stays writable and the write can retry.
        let _ = store.execute(
            META_DB,
            &format!(
                "UPDATE kv_shard SET state = 0, updated_at = CURRENT_TIMESTAMP WHERE id = {}",
                refreshed.id
            ),
        );
        return Err(err);
    }
    Ok(())
}

pub fn drop_shard_file(path: &str) {
    let base = format!("./database/{path}.db");
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{base}{suffix}"));
    }
}

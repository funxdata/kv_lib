use crate::error::Result;
use crate::store::Store;

pub const META_DB: &str = "kv";

pub fn create_schema(store: &Store) -> Result<()> {
    for statement in [
        "CREATE TABLE IF NOT EXISTS kv_collection (
            code           TEXT PRIMARY KEY NOT NULL,
            name           TEXT NOT NULL DEFAULT '',
            description    TEXT NOT NULL DEFAULT '',
            dim            INTEGER NOT NULL DEFAULT 1024,
            model          TEXT NOT NULL DEFAULT 'qwen3-embed',
            shard_max_mb   INTEGER NOT NULL DEFAULT 16,
            index_min_rows INTEGER NOT NULL DEFAULT 0,
            state          INTEGER NOT NULL DEFAULT 0,
            generation     INTEGER NOT NULL DEFAULT 0,
            sort           INTEGER NOT NULL DEFAULT 0,
            created_at     DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at     DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
        )",
        "CREATE TABLE IF NOT EXISTS kv_shard (
            id         INTEGER PRIMARY KEY AUTOINCREMENT,
            collection TEXT NOT NULL,
            shard_no   INTEGER NOT NULL,
            path       TEXT NOT NULL UNIQUE,
            row_count  INTEGER NOT NULL DEFAULT 0,
            size_bytes INTEGER NOT NULL DEFAULT 0,
            state      INTEGER NOT NULL DEFAULT 0,
            created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
            UNIQUE(collection, shard_no)
        )",
        "CREATE INDEX IF NOT EXISTS idx_kv_shard_active ON kv_shard(collection, state)",
    ] {
        store.execute(META_DB, statement)?;
    }

    store.execute(
        META_DB,
        "INSERT OR IGNORE INTO kv_collection (code, name, description, sort) VALUES
            ('prompt',   '提示词',   'system prompt 模板、规则与提示词条目', 10),
            ('plugin',   '插件能力', '插件能力描述向量',                     20),
            ('skill',    '技能',     '技能正文向量',                         30),
            ('material', '素材',     '素材内容文本向量',                     40)",
    )?;

    Ok(())
}

pub fn records_table_sql() -> &'static str {
    "CREATE TABLE IF NOT EXISTS kv_record (
        id         INTEGER PRIMARY KEY AUTOINCREMENT,
        key        TEXT NOT NULL,
        value      TEXT NOT NULL DEFAULT '',
        value_type TEXT NOT NULL DEFAULT 'text',
        text       TEXT NOT NULL DEFAULT '',
        text_hash  TEXT NOT NULL DEFAULT '',
        meta       TEXT NOT NULL DEFAULT '{}',
        rank       INTEGER NOT NULL DEFAULT 0,
        dim        INTEGER NOT NULL DEFAULT 0,
        vector     BLOB,
        version    INTEGER NOT NULL DEFAULT 1,
        created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
        updated_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
        UNIQUE(key)
    )"
}

/// 为已有分片补齐 `rank` 列（`CREATE TABLE IF NOT EXISTS` 不会修改旧表）。
pub fn migrate_schema(store: &Store) -> Result<()> {
    let rows = store.query(META_DB, "SELECT path FROM kv_shard")?;
    for row in rows {
        let path = crate::db::as_string(&row, "path");
        if path.is_empty() {
            continue;
        }
        let columns = store.query(&path, "PRAGMA table_info(kv_record)")?;
        let has_rank = columns
            .iter()
            .any(|column| crate::db::as_string(column, "name") == "rank");
        if !has_rank {
            store.execute(
                &path,
                "ALTER TABLE kv_record ADD COLUMN rank INTEGER NOT NULL DEFAULT 0",
            )?;
        }
    }
    Ok(())
}

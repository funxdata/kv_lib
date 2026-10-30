use crate::db::{as_i64, as_string, quote};
use crate::error::{KvError, Result};
use crate::store::Store;
use crate::store::schema::META_DB;
use crate::store::shard::{drop_shard_file, list_shards};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize)]
pub struct Collection {
    pub code: String,
    pub name: String,
    pub description: String,
    pub dim: i64,
    pub model: String,
    pub shard_max_mb: i64,
    pub index_min_rows: i64,
    pub state: i64,
    pub generation: i64,
    pub sort: i64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CollectionInfo {
    #[serde(flatten)]
    pub collection: Collection,
    pub row_count: i64,
    pub shard_count: i64,
    pub size_bytes: i64,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct CollectionInput {
    pub code: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub dim: Option<i64>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub shard_max_mb: Option<i64>,
    #[serde(default)]
    pub index_min_rows: Option<i64>,
    #[serde(default)]
    pub state: Option<i64>,
    #[serde(default)]
    pub sort: Option<i64>,
}

impl Collection {
    fn from_row(row: &Value) -> Self {
        Self {
            code: as_string(row, "code"),
            name: as_string(row, "name"),
            description: as_string(row, "description"),
            dim: as_i64(row, "dim"),
            model: as_string(row, "model"),
            shard_max_mb: as_i64(row, "shard_max_mb"),
            index_min_rows: as_i64(row, "index_min_rows"),
            state: as_i64(row, "state"),
            generation: as_i64(row, "generation"),
            sort: as_i64(row, "sort"),
            created_at: as_string(row, "created_at"),
            updated_at: as_string(row, "updated_at"),
        }
    }
}

pub fn validate_code(code: &str) -> Result<()> {
    let mut chars = code.chars();
    let Some(first) = chars.next() else {
        return Err(KvError::invalid_collection(
            "collection code must not be empty",
        ));
    };
    if !first.is_ascii_lowercase() {
        return Err(KvError::invalid_collection(
            "collection code must start with a lowercase letter",
        ));
    }
    if !chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-') {
        return Err(KvError::invalid_collection(
            "collection code may only contain lowercase letters, digits, '_' and '-'",
        ));
    }
    if code.len() > 64 {
        return Err(KvError::invalid_collection(
            "collection code must be at most 64 bytes",
        ));
    }
    Ok(())
}

pub fn validate(collection: &Collection) -> Result<()> {
    validate_code(&collection.code)?;
    if collection.dim <= 0 || collection.dim > 65536 {
        return Err(KvError::invalid_collection(
            "dim must be between 1 and 65536",
        ));
    }
    if collection.model.trim().is_empty() {
        return Err(KvError::invalid_collection("model must not be empty"));
    }
    if collection.shard_max_mb < 0 {
        return Err(KvError::invalid_collection("shard_max_mb must be >= 0"));
    }
    if collection.index_min_rows < 0 {
        return Err(KvError::invalid_collection("index_min_rows must be >= 0"));
    }
    if !(0..=2).contains(&collection.state) {
        return Err(KvError::invalid_collection("state must be 0, 1 or 2"));
    }
    Ok(())
}

pub fn try_get_collection(store: &Store, code: &str) -> Result<Option<Collection>> {
    store.ensure_schema()?;
    let rows = store.query(
        META_DB,
        &format!(
            "SELECT code, name, description, dim, model, shard_max_mb, index_min_rows, state,
                    generation, sort, created_at, updated_at
             FROM kv_collection WHERE code = {} LIMIT 1",
            quote(code)
        ),
    )?;
    Ok(rows.first().map(Collection::from_row))
}

pub fn get_collection(store: &Store, code: &str) -> Result<Collection> {
    try_get_collection(store, code)?.ok_or_else(|| KvError::unknown_collection(code))
}

pub fn list_collections(store: &Store) -> Result<Vec<CollectionInfo>> {
    store.ensure_schema()?;
    let rows = store.query(
        META_DB,
        "SELECT code, name, description, dim, model, shard_max_mb, index_min_rows, state,
                generation, sort, created_at, updated_at
         FROM kv_collection ORDER BY sort, code",
    )?;
    let shard_rows = store.query(
        META_DB,
        "SELECT collection, state, row_count, size_bytes FROM kv_shard",
    )?;

    let mut stats: std::collections::HashMap<String, (i64, i64, i64)> =
        std::collections::HashMap::new();
    for row in &shard_rows {
        let code = as_string(row, "collection");
        let entry = stats.entry(code).or_insert((0, 0, 0));
        entry.0 += as_i64(row, "row_count");
        entry.1 += 1;
        entry.2 += as_i64(row, "size_bytes");
    }

    Ok(rows
        .iter()
        .map(|row| {
            let collection = Collection::from_row(row);
            let (row_count, shard_count, size_bytes) =
                stats.get(&collection.code).copied().unwrap_or((0, 0, 0));
            CollectionInfo {
                collection,
                row_count,
                shard_count,
                size_bytes,
            }
        })
        .collect())
}

pub fn put_collection(store: &Store, input: CollectionInput) -> Result<(Collection, bool)> {
    validate_code(&input.code)?;
    let existing = try_get_collection(store, &input.code)?;
    let created = existing.is_none();
    let base = existing.clone().unwrap_or(Collection {
        code: input.code.clone(),
        name: input.code.clone(),
        description: String::new(),
        dim: 1024,
        model: "qwen3-embed".to_string(),
        shard_max_mb: 16,
        index_min_rows: 0,
        state: 0,
        generation: 0,
        sort: 0,
        created_at: String::new(),
        updated_at: String::new(),
    });

    let next = Collection {
        code: input.code.clone(),
        name: input.name.unwrap_or(base.name),
        description: input.description.unwrap_or(base.description),
        dim: input.dim.unwrap_or(base.dim),
        model: input.model.unwrap_or(base.model),
        shard_max_mb: input.shard_max_mb.unwrap_or(base.shard_max_mb),
        index_min_rows: input.index_min_rows.unwrap_or(base.index_min_rows),
        state: input.state.unwrap_or(base.state),
        generation: base.generation,
        sort: input.sort.unwrap_or(base.sort),
        created_at: base.created_at,
        updated_at: base.updated_at,
    };
    validate(&next)?;

    if let Some(old) = &existing {
        if old.dim != next.dim {
            let rows: i64 = list_shards(store, &next.code, true)?
                .iter()
                .map(|shard| shard.row_count)
                .sum();
            if rows > 0 {
                return Err(KvError::invalid_collection(
                    "cannot change dim while the collection contains records",
                ));
            }
        }
    }

    if created {
        store.execute(
            META_DB,
            &format!(
                "INSERT INTO kv_collection
                    (code, name, description, dim, model, shard_max_mb, index_min_rows, state, generation, sort)
                 VALUES ({}, {}, {}, {}, {}, {}, {}, {}, 0, {})",
                quote(&next.code),
                quote(&next.name),
                quote(&next.description),
                next.dim,
                quote(&next.model),
                next.shard_max_mb,
                next.index_min_rows,
                next.state,
                next.sort
            ),
        )?;
    } else {
        store.execute(
            META_DB,
            &format!(
                "UPDATE kv_collection SET
                    name = {}, description = {}, dim = {}, model = {},
                    shard_max_mb = {}, index_min_rows = {}, state = {}, sort = {},
                    updated_at = CURRENT_TIMESTAMP
                 WHERE code = {}",
                quote(&next.name),
                quote(&next.description),
                next.dim,
                quote(&next.model),
                next.shard_max_mb,
                next.index_min_rows,
                next.state,
                next.sort,
                quote(&next.code)
            ),
        )?;
    }

    let saved = get_collection(store, &next.code)?;
    if created || existing.is_some_and(|old| old.dim != saved.dim) {
        store.index().clear(&saved.code);
    }
    Ok((saved, created))
}

pub fn delete_collection(store: &Store, code: &str, hard: bool) -> Result<(i64, i64)> {
    let collection = get_collection(store, code).map_err(|_| KvError::not_found(code))?;
    let shards = list_shards(store, &collection.code, true)?;
    let mut records = 0i64;
    for shard in &shards {
        let rows = store.query(&shard.path, "SELECT COUNT(*) AS n FROM kv_record")?;
        records += rows
            .first()
            .map(|row| as_i64(row, "n"))
            .unwrap_or(shard.row_count);
    }
    if records > 0 && !hard {
        return Err(KvError::new(
            400605,
            "collection is not empty; pass hard=true to delete it",
        ));
    }

    for shard in &shards {
        drop_shard_file(&shard.path);
        store.execute(
            META_DB,
            &format!("DELETE FROM kv_shard WHERE id = {}", shard.id),
        )?;
    }
    store.execute(
        META_DB,
        &format!(
            "DELETE FROM kv_collection WHERE code = {}",
            quote(&collection.code)
        ),
    )?;
    store.index().clear(&collection.code);
    Ok((shards.len() as i64, records))
}

pub fn bump_generation(store: &Store, code: &str) -> Result<()> {
    store.execute(
        META_DB,
        &format!(
            "UPDATE kv_collection SET generation = generation + 1, updated_at = CURRENT_TIMESTAMP
             WHERE code = {}",
            quote(code)
        ),
    )?;
    store.index().invalidate(code);
    Ok(())
}

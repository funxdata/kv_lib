use crate::db::{as_i64, as_optional_string, as_string, in_list, quote};
use crate::error::{KvError, Result};
use crate::store::Store;
use crate::store::collection::{Collection, bump_generation, get_collection};
use crate::store::shard::{Shard, ensure_active_shard, list_shards, maybe_seal, refresh_shard};
use crate::utils::hex;
use crate::utils::text_hash::sha256_hex;
use serde_json::{Value, json};

const MAX_VALUE_BYTES: usize = 256 * 1024;
const MAX_TEXT_BYTES: usize = 64 * 1024;
const MAX_META_BYTES: usize = 8 * 1024;
const MAX_BATCH: usize = 500;

#[derive(Debug, Clone, Default)]
pub struct PutItem {
    pub key: String,
    pub value: Option<String>,
    pub value_type: Option<String>,
    pub text: Option<String>,
    pub text_hash: Option<String>,
    pub meta: Option<Value>,
    /// 业务排名：数值越小越靠前，`0` 表示未排名。
    pub rank: Option<i64>,
    pub vector: Option<Vec<f32>>,
    pub dim: Option<i64>,
    pub force: bool,
    pub if_version: Option<i64>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ItemError {
    pub key: String,
    pub code: i32,
    pub message: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PutResult {
    pub written: usize,
    pub skipped: usize,
    pub failed: usize,
    pub errors: Vec<ItemError>,
    pub model: String,
    pub dim: i64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DeleteResult {
    pub deleted: usize,
    pub requested: usize,
    pub missing: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Record {
    pub key: String,
    pub value: String,
    pub value_type: String,
    pub text: String,
    pub text_hash: String,
    pub meta: Value,
    pub rank: i64,
    pub dim: i64,
    pub vector: Option<Vec<f32>>,
    pub version: i64,
    pub created_at: String,
    pub updated_at: String,
}

impl Record {
    pub fn to_json(&self, with_value: bool, with_vector: bool) -> Value {
        let mut object = serde_json::Map::new();
        object.insert("key".into(), json!(self.key));
        object.insert("value_type".into(), json!(self.value_type));
        if with_value {
            object.insert("value".into(), json!(self.value));
            object.insert("meta".into(), self.meta.clone());
        }
        object.insert("version".into(), json!(self.version));
        object.insert("rank".into(), json!(self.rank));
        object.insert("dim".into(), json!(self.dim));
        object.insert("has_vector".into(), json!(self.vector.is_some()));
        object.insert("text_hash".into(), json!(self.text_hash));
        object.insert("created_at".into(), json!(self.created_at));
        object.insert("updated_at".into(), json!(self.updated_at));
        if with_vector {
            object.insert(
                "vector".into(),
                match &self.vector {
                    Some(vector) => json!(hex::encode(vector)),
                    None => Value::Null,
                },
            );
        }
        Value::Object(object)
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct GetResult {
    pub list: Vec<Value>,
    pub missing: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ListResult {
    pub list: Vec<Value>,
    pub page: usize,
    pub page_size: usize,
    pub total: i64,
}

pub fn validate_key(key: &str) -> Result<()> {
    if key.is_empty() || key.len() > 128 {
        return Err(KvError::invalid_key(key));
    }
    let mut chars = key.chars();
    let first = chars.next().expect("non-empty");
    if !first.is_ascii_alphanumeric() {
        return Err(KvError::invalid_key(key));
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == ':' || c == '-') {
        return Err(KvError::invalid_key(key));
    }
    Ok(())
}

pub fn validate_value_type(value_type: &str) -> Result<()> {
    if value_type != "text" && value_type != "json" {
        return Err(KvError::value_too_large(format!(
            "value_type must be text or json, got {value_type}"
        )));
    }
    Ok(())
}

/// rank 排序键：正数按升序，`0` / 负数视为未排名，排在最后。
pub(crate) fn rank_order(rank: i64) -> (u8, i64) {
    if rank > 0 { (0, rank) } else { (1, 0) }
}

fn record_from_row(row: &Value) -> Result<Record> {
    let vector_hex = as_optional_string(row, "vector_hex");
    let vector = match vector_hex {
        Some(text) => Some(hex::decode(&text)?),
        None => None,
    };
    let meta_text = as_string(row, "meta");
    let meta = if meta_text.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(&meta_text).unwrap_or_else(|_| json!({}))
    };
    let record = Record {
        key: as_string(row, "key"),
        value: as_string(row, "value"),
        value_type: as_string(row, "value_type"),
        text: as_string(row, "text"),
        text_hash: as_string(row, "text_hash"),
        meta,
        rank: as_i64(row, "rank"),
        dim: as_i64(row, "dim"),
        vector,
        version: as_i64(row, "version"),
        created_at: as_string(row, "created_at"),
        updated_at: as_string(row, "updated_at"),
    };
    Ok(record)
}

/// 只查询需要的列；`with_vector=false` 时不读取 vector，避免大集合的 hex 文本
/// 超过 database FFI 响应缓冲区。
fn select_columns(with_vector: bool) -> &'static str {
    if with_vector {
        "key, value, value_type, text, text_hash, meta, rank, dim,
         CASE WHEN vector IS NULL THEN NULL ELSE hex(vector) END AS vector_hex,
         version, created_at, updated_at"
    } else {
        "key, value, value_type, text, text_hash, meta, rank, dim,
         version, created_at, updated_at"
    }
}

fn find_in_shards(
    store: &Store,
    collection: &Collection,
    key: &str,
) -> Result<Option<(Shard, Record)>> {
    for shard in list_shards(store, &collection.code, false)? {
        let rows = store.query(
            &shard.path,
            &format!(
                "SELECT {} FROM kv_record WHERE key = {} LIMIT 1",
                select_columns(true),
                quote(key)
            ),
        )?;
        if let Some(row) = rows.first() {
            let record = record_from_row(row)?;
            return Ok(Some((shard, record)));
        }
    }
    Ok(None)
}

pub fn get_records(
    store: &Store,
    collection_code: &str,
    keys: &[String],
    with_vector: bool,
) -> Result<GetResult> {
    store.ensure_schema()?;
    let collection = get_collection(store, collection_code)?;
    if keys.len() > MAX_BATCH {
        return Err(KvError::bad_request(format!(
            "at most {MAX_BATCH} keys per request"
        )));
    }
    for key in keys {
        validate_key(key)?;
    }

    let mut found: std::collections::HashMap<String, Record> = std::collections::HashMap::new();
    if !keys.is_empty() {
        let list = in_list(keys);
        for shard in list_shards(store, &collection.code, false)? {
            let rows = store.query(
                &shard.path,
                &format!(
                    "SELECT {} FROM kv_record WHERE key IN ({})",
                    select_columns(with_vector),
                    list
                ),
            )?;
            for row in &rows {
                let record = record_from_row(row)?;
                found.insert(record.key.clone(), record);
            }
        }
    }

    let mut list = Vec::new();
    let mut missing = Vec::new();
    for key in keys {
        match found.get(key) {
            Some(record) => list.push(record.to_json(true, with_vector)),
            None => missing.push(key.clone()),
        }
    }
    Ok(GetResult { list, missing })
}

pub fn put_records(store: &Store, collection_code: &str, items: &[PutItem]) -> Result<PutResult> {
    store.ensure_schema()?;
    let collection = get_collection(store, collection_code)?;
    if collection.state != 0 {
        return Err(KvError::readonly_collection(&collection.code));
    }
    if items.len() > MAX_BATCH {
        return Err(KvError::bad_request(format!(
            "at most {MAX_BATCH} items per request"
        )));
    }
    let active = ensure_active_shard(store, &collection)?;

    let mut result = PutResult {
        written: 0,
        skipped: 0,
        failed: 0,
        errors: Vec::new(),
        model: store.embedder().model().to_string(),
        dim: collection.dim,
    };
    let mut touched: std::collections::HashMap<String, Shard> = std::collections::HashMap::new();

    for item in items {
        match put_one(store, &collection, &active, item, &mut touched) {
            Ok(PutOutcome::Written) => result.written += 1,
            Ok(PutOutcome::Skipped) => result.skipped += 1,
            Ok(PutOutcome::WrittenWithoutVector(err)) => {
                result.written += 1;
                result.errors.push(ItemError {
                    key: item.key.clone(),
                    code: err.code,
                    message: err.msg,
                });
            }
            Err(err) => {
                result.failed += 1;
                result.errors.push(ItemError {
                    key: item.key.clone(),
                    code: err.code,
                    message: err.msg,
                });
            }
        }
    }

    for shard in touched.values() {
        let _ = refresh_shard(store, shard);
    }
    if result.written > 0 {
        bump_generation(store, &collection.code)?;
        maybe_seal(store, &collection)?;
    }
    Ok(result)
}

enum PutOutcome {
    Written,
    Skipped,
    WrittenWithoutVector(KvError),
}

fn put_one(
    store: &Store,
    collection: &Collection,
    active: &Shard,
    item: &PutItem,
    touched: &mut std::collections::HashMap<String, Shard>,
) -> Result<PutOutcome> {
    validate_key(&item.key)?;

    let existing = find_in_shards(store, collection, &item.key)?;
    let (target, old) = match existing {
        Some((shard, record)) => (shard, Some(record)),
        None => (active.clone(), None),
    };

    if let (Some(expected), Some(record)) = (item.if_version, old.as_ref()) {
        if expected != record.version {
            return Err(KvError::version_conflict(&item.key));
        }
    }

    let value = item
        .value
        .clone()
        .or_else(|| old.as_ref().map(|record| record.value.clone()))
        .unwrap_or_default();
    let value_type = item
        .value_type
        .clone()
        .or_else(|| old.as_ref().map(|record| record.value_type.clone()))
        .unwrap_or_else(|| "text".to_string());
    validate_value_type(&value_type)?;
    if value.len() > MAX_VALUE_BYTES {
        return Err(KvError::value_too_large(format!(
            "value exceeds {MAX_VALUE_BYTES} bytes"
        )));
    }
    if value_type == "json" && !value.trim().is_empty() {
        serde_json::from_str::<Value>(&value)
            .map_err(|err| KvError::value_too_large(format!("value is not valid JSON: {err}")))?;
    }
    let meta = item
        .meta
        .clone()
        .or_else(|| old.as_ref().map(|record| record.meta.clone()))
        .unwrap_or_else(|| json!({}));
    let meta_text =
        serde_json::to_string(&meta).map_err(|err| KvError::database(err.to_string()))?;
    if meta_text.len() > MAX_META_BYTES {
        return Err(KvError::value_too_large(format!(
            "meta exceeds {MAX_META_BYTES} bytes"
        )));
    }
    let rank = item
        .rank
        .or_else(|| old.as_ref().map(|record| record.rank))
        .unwrap_or(0);

    let mut text = old
        .as_ref()
        .map(|record| record.text.clone())
        .unwrap_or_default();
    let mut text_hash = old
        .as_ref()
        .map(|record| record.text_hash.clone())
        .unwrap_or_default();
    let mut vector = old.as_ref().and_then(|record| record.vector.clone());
    let mut dim = old.as_ref().map(|record| record.dim).unwrap_or(0);
    let mut embed_error = None;

    if let Some(new_text) = &item.text {
        if new_text.len() > MAX_TEXT_BYTES {
            return Err(KvError::value_too_large(format!(
                "text exceeds {MAX_TEXT_BYTES} bytes"
            )));
        }
        if new_text.is_empty() {
            text.clear();
            text_hash.clear();
            vector = None;
            dim = 0;
        } else {
            text = new_text.clone();
            text_hash = sha256_hex(new_text);
        }
    }
    if let Some(hash) = &item.text_hash {
        text_hash = hash.clone();
    }

    if let Some(supplied) = &item.vector {
        let normalized = hex::normalize(supplied.clone())?;
        if normalized.len() as i64 != collection.dim {
            return Err(KvError::invalid_vector(format!(
                "vector dimension mismatch: expected {}, got {}",
                collection.dim,
                normalized.len()
            )));
        }
        vector = Some(normalized);
        dim = collection.dim;
    } else if !text.is_empty()
        && (item.force
            || vector.is_none()
            || old
                .as_ref()
                .is_none_or(|record| record.text_hash != text_hash))
    {
        match store.embedder().embed(&text) {
            Ok(embedded) => match hex::normalize(embedded) {
                Ok(normalized) if normalized.len() as i64 == collection.dim => {
                    vector = Some(normalized);
                    dim = collection.dim;
                }
                Ok(normalized) => {
                    embed_error = Some(KvError::invalid_vector(format!(
                        "embedding dimension mismatch: expected {}, got {}",
                        collection.dim,
                        normalized.len()
                    )));
                    vector = None;
                    dim = 0;
                }
                Err(err) => {
                    embed_error = Some(err);
                    vector = None;
                    dim = 0;
                }
            },
            Err(err) => {
                embed_error = Some(KvError::embedding(format!("{err:#}")));
                vector = None;
                dim = 0;
            }
        }
    }

    let changed = match &old {
        Some(record) => {
            record.value != value
                || record.value_type != value_type
                || record.text != text
                || record.meta != meta
                || record.rank != rank
                || item.vector.is_some()
        }
        None => true,
    };
    if !changed && embed_error.is_none() {
        return Ok(PutOutcome::Skipped);
    }

    let version = old.as_ref().map(|record| record.version + 1).unwrap_or(1);
    let vector_sql = match &vector {
        Some(vector) => format!("X'{}'", hex::encode(vector)),
        None => "NULL".to_string(),
    };
    let sql = format!(
        "INSERT INTO kv_record
            (key, value, value_type, text, text_hash, meta, rank, dim, vector, version)
         VALUES ({}, {}, {}, {}, {}, {}, {}, {}, {}, {})
         ON CONFLICT(key) DO UPDATE SET
            value = excluded.value,
            value_type = excluded.value_type,
            text = excluded.text,
            text_hash = excluded.text_hash,
            meta = excluded.meta,
            rank = excluded.rank,
            dim = excluded.dim,
            vector = excluded.vector,
            version = excluded.version,
            updated_at = CURRENT_TIMESTAMP",
        quote(&item.key),
        quote(&value),
        quote(&value_type),
        quote(&text),
        quote(&text_hash),
        quote(&meta_text),
        rank,
        dim,
        vector_sql,
        version
    );
    store
        .execute(&target.path, &sql)
        .map_err(|err| KvError::database(format!("write {} failed: {}", target.path, err.msg)))?;
    touched.insert(target.path.clone(), target);

    Ok(match embed_error {
        Some(err) => PutOutcome::WrittenWithoutVector(err),
        None => PutOutcome::Written,
    })
}

pub fn delete_records(
    store: &Store,
    collection_code: &str,
    keys: &[String],
    delete_vector_only: bool,
) -> Result<DeleteResult> {
    store.ensure_schema()?;
    let collection = get_collection(store, collection_code)?;
    if keys.len() > MAX_BATCH {
        return Err(KvError::bad_request(format!(
            "at most {MAX_BATCH} keys per request"
        )));
    }
    for key in keys {
        validate_key(key)?;
    }

    let deleted;
    let mut found = std::collections::HashSet::new();
    if keys.is_empty() {
        return Ok(DeleteResult {
            deleted: 0,
            requested: 0,
            missing: Vec::new(),
        });
    }
    let list = in_list(keys);
    for shard in list_shards(store, &collection.code, false)? {
        let rows = store.query(
            &shard.path,
            &format!("SELECT key FROM kv_record WHERE key IN ({list})"),
        )?;
        for row in &rows {
            found.insert(as_string(row, "key"));
        }
        if delete_vector_only {
            store.execute(
                &shard.path,
                &format!(
                    "UPDATE kv_record SET dim = 0, vector = NULL, text_hash = '', updated_at = CURRENT_TIMESTAMP
                     WHERE key IN ({list})"
                ),
            )?;
        } else {
            store.execute(
                &shard.path,
                &format!("DELETE FROM kv_record WHERE key IN ({list})"),
            )?;
        }
        let _ = refresh_shard(store, &shard);
    }

    deleted = found.len();
    if deleted > 0 {
        bump_generation(store, &collection.code)?;
    }
    let missing = keys
        .iter()
        .filter(|key| !found.contains(*key))
        .cloned()
        .collect();
    Ok(DeleteResult {
        deleted,
        requested: keys.len(),
        missing,
    })
}

pub fn list_records(
    store: &Store,
    collection_code: &str,
    prefix: Option<&str>,
    page: usize,
    page_size: usize,
    with_value: bool,
    with_vector: bool,
) -> Result<ListResult> {
    store.ensure_schema()?;
    let collection = get_collection(store, collection_code)?;
    if page == 0 || page_size == 0 || page_size > 100 {
        return Err(KvError::bad_request(
            "page must be >= 1 and page_size must be between 1 and 100",
        ));
    }
    let offset = (page - 1) * page_size;
    let filter = match prefix {
        Some(prefix) if !prefix.is_empty() => {
            format!(
                "WHERE key LIKE {}",
                quote(&format!("{}%", prefix.replace('%', "\\%")))
            )
        }
        _ => String::new(),
    };

    let mut rows = Vec::new();
    let mut total = 0i64;
    for shard in list_shards(store, &collection.code, false)? {
        let count = store.query(
            &shard.path,
            &format!("SELECT COUNT(*) AS n FROM kv_record {filter}"),
        )?;
        total += count.first().map(|row| as_i64(row, "n")).unwrap_or(0);
        let result = store.query(
            &shard.path,
            &format!(
                "SELECT {} FROM kv_record {filter}
                 ORDER BY CASE WHEN rank <= 0 THEN 1 ELSE 0 END, rank, key
                 LIMIT {page_size} OFFSET {offset}",
                select_columns(with_vector)
            ),
        )?;
        for row in &result {
            let record = record_from_row(row)?;
            rows.push(record);
        }
    }
    rows.sort_by(|a, b| {
        rank_order(a.rank)
            .cmp(&rank_order(b.rank))
            .then_with(|| a.key.cmp(&b.key))
    });
    rows.truncate(page_size);

    Ok(ListResult {
        list: rows
            .iter()
            .map(|record| record.to_json(with_value, with_vector))
            .collect(),
        page,
        page_size,
        total,
    })
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_columns_skip_vector_when_not_requested() {
        assert!(!select_columns(false).contains("vector_hex"));
        assert!(select_columns(true).contains("vector_hex"));
    }
}

use crate::db::{as_i64, as_string, in_list, quote};
use crate::error::{KvError, Result};
use crate::store::Store;
use crate::store::collection::{bump_generation, get_collection, validate_code};
use crate::store::record::validate_key;
use crate::store::shard::{list_shards, refresh_shard};
use crate::utils::hex;
use crate::utils::text_hash::sha256_hex;
use serde::Serialize;
use std::collections::HashMap;
use std::time::Instant;

#[derive(Debug, Clone, Serialize)]
pub struct EmbedResult {
    pub processed: usize,
    pub skipped: usize,
    pub failed: usize,
    pub model: String,
    pub dim: i64,
    pub elapsed_ms: u64,
}

struct Pending {
    key: String,
    text: String,
    path: String,
}

pub fn embed_records(
    store: &Store,
    collection_code: &str,
    keys: Option<&[String]>,
    limit: Option<usize>,
    offset: Option<usize>,
    force: bool,
) -> Result<EmbedResult> {
    let started = Instant::now();
    store.ensure_schema()?;
    validate_code(collection_code)?;
    let collection = get_collection(store, collection_code)?;
    if collection.state != 0 {
        return Err(KvError::readonly_collection(&collection.code));
    }
    if let Some(keys) = keys {
        for key in keys {
            validate_key(key)?;
        }
    }

    let key_filter = keys
        .filter(|keys| !keys.is_empty())
        .map(|keys| format!(" AND key IN ({})", in_list(keys)))
        .unwrap_or_default();

    let mut pending = Vec::new();
    let mut total = 0usize;
    for shard in list_shards(store, &collection.code, false)? {
        let rows = store.query(
            &shard.path,
            &format!(
                "SELECT key, text, text_hash, CASE WHEN vector IS NULL THEN 0 ELSE 1 END AS has_vector
                 FROM kv_record WHERE text != ''{key_filter} ORDER BY key"
            ),
        )?;
        for row in &rows {
            total += 1;
            let text = as_string(row, "text");
            let old_hash = as_string(row, "text_hash");
            let has_vector = as_i64(row, "has_vector") == 1;
            let hash = sha256_hex(&text);
            if force || !has_vector || old_hash != hash {
                pending.push(Pending {
                    key: as_string(row, "key"),
                    text,
                    path: shard.path.clone(),
                });
            }
        }
    }

    let offset = offset.unwrap_or(0);
    let skipped = total.saturating_sub(pending.len());
    if offset >= pending.len() {
        return Ok(EmbedResult {
            processed: 0,
            skipped,
            failed: 0,
            model: store.embedder().model().to_string(),
            dim: collection.dim,
            elapsed_ms: started.elapsed().as_millis() as u64,
        });
    }
    pending.drain(0..offset);
    if let Some(limit) = limit {
        if limit > 0 {
            pending.truncate(limit);
        }
    }

    let mut processed = 0usize;
    let mut failed = 0usize;
    let mut touched: HashMap<String, crate::store::Shard> = HashMap::new();
    let mut first_error: Option<KvError> = None;

    for item in pending {
        let vector = match store.embedder().embed(&item.text) {
            Ok(vector) => match hex::normalize(vector) {
                Ok(vector) if vector.len() as i64 == collection.dim => vector,
                Ok(vector) => {
                    failed += 1;
                    first_error.get_or_insert_with(|| {
                        KvError::invalid_vector(format!(
                            "embedding dimension mismatch: expected {}, got {}",
                            collection.dim,
                            vector.len()
                        ))
                    });
                    continue;
                }
                Err(err) => {
                    failed += 1;
                    first_error.get_or_insert(err);
                    continue;
                }
            },
            Err(err) => {
                failed += 1;
                first_error.get_or_insert_with(|| KvError::embedding(format!("{err:#}")));
                continue;
            }
        };
        let hash = sha256_hex(&item.text);
        let sql = format!(
            "UPDATE kv_record SET dim = {}, vector = X'{}', text_hash = {}, updated_at = CURRENT_TIMESTAMP
             WHERE key = {}",
            collection.dim,
            hex::encode(&vector),
            quote(&hash),
            quote(&item.key)
        );
        match store.execute(&item.path, &sql) {
            Ok(()) => {
                processed += 1;
                if let Some(shard) = list_shards(store, &collection.code, false)?
                    .into_iter()
                    .find(|shard| shard.path == item.path)
                {
                    touched.insert(item.path.clone(), shard);
                }
            }
            Err(err) => {
                failed += 1;
                first_error.get_or_insert(err);
            }
        }
    }

    for shard in touched.values() {
        let _ = refresh_shard(store, shard);
    }
    if processed > 0 {
        bump_generation(store, &collection.code)?;
    }

    if processed == 0 && failed > 0 {
        if let Some(err) = first_error {
            return Err(err);
        }
    }

    Ok(EmbedResult {
        processed,
        skipped,
        failed,
        model: store.embedder().model().to_string(),
        dim: collection.dim,
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}

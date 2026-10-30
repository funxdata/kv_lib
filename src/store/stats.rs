use crate::db::{as_i64, as_string, quote};
use crate::error::{KvError, Result};
use crate::store::Store;
use crate::store::collection::{CollectionInfo, get_collection, list_collections};
use crate::store::record::validate_key;
use crate::store::shard::{list_shards, refresh_shard};
use crate::utils::text_hash::sha256_hex;
use serde_json::{Value, json};
use std::time::Instant;

pub fn stats(store: &Store, collection: Option<&str>) -> Result<Value> {
    store.ensure_schema()?;
    let infos: Vec<CollectionInfo> = match collection {
        Some(code) => {
            let target = get_collection(store, code)?;
            list_collections(store)?
                .into_iter()
                .filter(|info| info.collection.code == target.code)
                .collect()
        }
        None => list_collections(store)?,
    };

    let mut list = Vec::new();
    let mut total_shards = 0i64;
    let mut total_rows = 0i64;
    let mut total_vectors = 0i64;
    let mut total_size = 0i64;
    let mut cached = 0usize;
    let mut stale = 0usize;

    for info in infos {
        let shards = list_shards(store, &info.collection.code, true)?;
        let mut vectors = 0i64;
        for shard in &shards {
            if shard.state == 2 {
                continue;
            }
            let rows = store.query(
                &shard.path,
                "SELECT COUNT(*) AS n FROM kv_record WHERE vector IS NOT NULL",
            )?;
            vectors += rows.first().map(|row| as_i64(row, "n")).unwrap_or(0);
        }
        let is_cached = store
            .index()
            .is_cached(&info.collection.code, info.collection.generation);
        if is_cached {
            cached += 1;
        }
        let index_state = if is_cached {
            "fresh"
        } else if info.row_count > 0 && info.collection.index_min_rows <= info.row_count {
            "stale"
        } else {
            "none"
        };
        if index_state == "stale" {
            stale += 1;
        }

        total_shards += info.shard_count;
        total_rows += info.row_count;
        total_vectors += vectors;
        total_size += info.size_bytes;

        list.push(json!({
            "code": info.collection.code,
            "rows": info.row_count,
            "shards": info.shard_count,
            "vectors": vectors,
            "size_bytes": info.size_bytes,
            "generation": info.collection.generation,
            "index": index_state,
        }));
    }

    Ok(json!({
        "collections": list,
        "totals": {
            "collections": list.len() as i64,
            "shards": total_shards,
            "rows": total_rows,
            "vectors": total_vectors,
            "size_bytes": total_size,
        },
        "index": { "cached": cached, "stale": stale },
    }))
}

pub fn rebuild(store: &Store, collection_code: &str, action: &str, force: bool) -> Result<Value> {
    let started = Instant::now();
    store.ensure_schema()?;
    let collection = get_collection(store, collection_code)?;
    let mut processed = 0i64;

    match action {
        "index" => {
            store.index().clear(&collection.code);
            if force {
                store.index().rebuild(store, &collection)?;
            }
        }
        "shards" => {
            for shard in list_shards(store, &collection.code, true)? {
                if shard.state == 2 {
                    continue;
                }
                let _ = refresh_shard(store, &shard)?;
                processed += 1;
            }
        }
        "text_hash" => {
            for shard in list_shards(store, &collection.code, false)? {
                let rows = store.query(
                    &shard.path,
                    "SELECT key, text FROM kv_record WHERE text != '' ORDER BY key",
                )?;
                for row in &rows {
                    let key = as_string(row, "key");
                    let text = as_string(row, "text");
                    validate_key(&key)?;
                    let hash = sha256_hex(&text);
                    store.execute(
                        &shard.path,
                        &format!(
                            "UPDATE kv_record SET text_hash = {}, updated_at = CURRENT_TIMESTAMP WHERE key = {}",
                            quote(&hash),
                            quote(&key)
                        ),
                    )?;
                    processed += 1;
                }
            }
        }
        _ => {
            return Err(KvError::invalid_search(
                "action must be index, shards or text_hash",
            ));
        }
    }

    Ok(json!({
        "collection": collection.code,
        "action": action,
        "rebuilt": true,
        "processed": processed,
        "elapsed_ms": started.elapsed().as_millis() as u64,
    }))
}

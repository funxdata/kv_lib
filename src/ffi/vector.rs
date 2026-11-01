use crate::error::KvError;
use crate::ffi::{from_value, handle, parse, parse_vector};
use crate::search::{prepare_query, search};
use crate::store::{embed_records, get_collection, get_records, rebuild};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::ffi::c_char;
use std::os::raw::c_int;

#[derive(Debug, Deserialize)]
struct EmbedReq {
    collection: String,
    #[serde(default)]
    keys: Option<Vec<String>>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    offset: Option<usize>,
    #[serde(default)]
    force: bool,
}

#[derive(Debug, Deserialize)]
struct SearchReq {
    collection: String,
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    vector: Option<Value>,
    #[serde(default)]
    top_k: Option<usize>,
    #[serde(default)]
    min_score: Option<f32>,
    #[serde(default)]
    candidate_keys: Option<Vec<String>>,
    #[serde(default)]
    with_value: bool,
}

/// 向量化与重建。
///
/// 入参 `action`：`embed`（默认）/ `rebuild`；
/// `rebuild` 时用 `target` 指定 `index`（默认）/ `shards` / `text_hash`。
#[unsafe(no_mangle)]
pub extern "C" fn post_kv_embed(json_in: *const c_char, json_out: *mut c_char) -> c_int {
    handle(json_in, json_out, |store, body| {
        let request: Value = parse(body)?;
        let action = request
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("embed");

        match action {
            "embed" => {
                let req: EmbedReq = from_value(request)?;
                let result = embed_records(
                    store,
                    &req.collection,
                    req.keys.as_deref(),
                    req.limit,
                    req.offset,
                    req.force,
                )?;
                Ok(serde_json::to_value(result).unwrap_or(Value::Null))
            }
            "rebuild" => {
                let collection = request
                    .get("collection")
                    .and_then(Value::as_str)
                    .filter(|code| !code.trim().is_empty())
                    .ok_or_else(|| KvError::invalid_collection("collection is required"))?;
                let target = request
                    .get("target")
                    .and_then(Value::as_str)
                    .unwrap_or("index");
                let force = request.get("force").and_then(Value::as_bool).unwrap_or(false);
                rebuild(store, collection, target, force)
            }
            other => Err(KvError::bad_request(format!("unknown action: {other}"))),
        }
    })
}

/// 相似度检索：`query`（文本）或 `vector`（hex / 数组）二选一。
#[unsafe(no_mangle)]
pub extern "C" fn post_kv_search(json_in: *const c_char, json_out: *mut c_char) -> c_int {
    handle(json_in, json_out, |store, body| {
        let req: SearchReq = from_value(parse(body)?)?;
        let collection = get_collection(store, &req.collection)?;
        let query_text = req
            .query
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty());

        let query_vector = if let Some(text) = query_text {
            let embedded = store
                .embedder()
                .embed(text)
                .map_err(|err| KvError::embedding(format!("{err:#}")))?;
            prepare_query(embedded, collection.dim)?
        } else if let Some(value) = &req.vector {
            prepare_query(parse_vector(value)?, collection.dim)?
        } else {
            return Err(KvError::invalid_search("query or vector is required"));
        };

        let top_k = req.top_k.unwrap_or(5).clamp(1, 100);
        let min_score = req.min_score.unwrap_or(0.0);
        let hits = search(
            store,
            &req.collection,
            query_vector,
            top_k,
            min_score,
            req.candidate_keys.as_deref(),
        )?;

        let values = if req.with_value && !hits.is_empty() {
            let keys: Vec<String> = hits.iter().map(|hit| hit.key.clone()).collect();
            let result = get_records(store, &req.collection, &keys, false)?;
            let mut map = HashMap::new();
            for item in result.list {
                if let Some(key) = item.get("key").and_then(Value::as_str) {
                    map.insert(key.to_string(), item);
                }
            }
            Some(map)
        } else {
            None
        };

        let list: Vec<Value> = hits
            .into_iter()
            .map(|hit| {
                let mut object = json!({ "key": hit.key, "score": hit.score, "rank": hit.rank });
                if let Some(values) = &values {
                    if let Some(item) = values.get(&hit.key) {
                        if let (Some(object), Some(item)) =
                            (object.as_object_mut(), item.as_object())
                        {
                            for key in ["value", "value_type", "meta"] {
                                if let Some(value) = item.get(key) {
                                    object.insert(key.to_string(), value.clone());
                                }
                            }
                        }
                    }
                }
                object
            })
            .collect();

        Ok(json!({
            "collection": req.collection,
            "model": store.embedder().model(),
            "dim": collection.dim,
            "query": query_text,
            "top_k": top_k,
            "list": list,
        }))
    })
}

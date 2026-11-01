use crate::error::KvError;
use crate::ffi::{from_value, handle, parse};
use crate::store::{CollectionInput, delete_collection, list_collections, put_collection, stats};
use serde_json::{Value, json};
use std::ffi::c_char;
use std::os::raw::c_int;

/// 集合操作。
///
/// 入参 `action`：`list`（默认）/ `put` / `delete` / `stats`。
#[unsafe(no_mangle)]
pub extern "C" fn post_kv_collection(json_in: *const c_char, json_out: *mut c_char) -> c_int {
    handle(json_in, json_out, |store, body| {
        let request: Value = parse(body)?;
        let action = request
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("list");

        match action {
            "list" => {
                let page = request
                    .get("page")
                    .and_then(Value::as_u64)
                    .unwrap_or(1)
                    .max(1) as usize;
                let page_size = request
                    .get("page_size")
                    .and_then(Value::as_u64)
                    .unwrap_or(20)
                    .clamp(1, 100) as usize;
                let all = list_collections(store)?;
                let total = all.len();
                let list: Vec<Value> = all
                    .into_iter()
                    .skip((page - 1) * page_size)
                    .take(page_size)
                    .map(|info| serde_json::to_value(info).unwrap_or(Value::Null))
                    .collect();
                Ok(json!({
                    "list": list,
                    "page": page,
                    "page_size": page_size,
                    "total": total,
                }))
            }
            "put" => {
                let input: CollectionInput = from_value(request)?;
                let (collection, created) = put_collection(store, input)?;
                Ok(json!({ "code": collection.code, "created": created }))
            }
            "delete" => {
                let code = request
                    .get("code")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if code.is_empty() {
                    return Err(KvError::invalid_collection("code is required"));
                }
                let hard = request.get("hard").and_then(Value::as_bool).unwrap_or(false);
                let (shards, records) = delete_collection(store, &code, hard)?;
                Ok(json!({
                    "code": code,
                    "deleted_shards": shards,
                    "deleted_records": records,
                }))
            }
            "stats" => {
                let collection = request.get("collection").and_then(Value::as_str);
                stats(store, collection)
            }
            other => Err(KvError::bad_request(format!("unknown action: {other}"))),
        }
    })
}

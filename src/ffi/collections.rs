use crate::error::KvError;
use crate::ffi::{handle, parse};
use crate::store::{CollectionInput, delete_collection, list_collections, put_collection};
use serde::Deserialize;
use serde_json::{Value, json};
use std::ffi::c_char;
use std::os::raw::c_int;

#[derive(Debug, Deserialize, Default)]
struct ListReq {
    #[serde(default)]
    page: Option<usize>,
    #[serde(default)]
    page_size: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct DeleteReq {
    code: String,
    #[serde(default)]
    hard: bool,
}

#[unsafe(no_mangle)]
pub extern "C" fn post_kv_collection_list(json_in: *const c_char, json_out: *mut c_char) -> c_int {
    handle(json_in, json_out, |store, body| {
        let req: ListReq = parse(body)?;
        let page = req.page.unwrap_or(1).max(1);
        let page_size = req.page_size.unwrap_or(20).clamp(1, 100);
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
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn post_kv_collection_put(json_in: *const c_char, json_out: *mut c_char) -> c_int {
    handle(json_in, json_out, |store, body| {
        let input: CollectionInput = parse(body)?;
        let (collection, created) = put_collection(store, input)?;
        Ok(json!({ "code": collection.code, "created": created }))
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn post_kv_collection_delete(
    json_in: *const c_char,
    json_out: *mut c_char,
) -> c_int {
    handle(json_in, json_out, |store, body| {
        let req: DeleteReq = parse(body)?;
        if req.code.trim().is_empty() {
            return Err(KvError::invalid_collection("code is required"));
        }
        let (shards, records) = delete_collection(store, &req.code, req.hard)?;
        Ok(json!({
            "code": req.code,
            "deleted_shards": shards,
            "deleted_records": records,
        }))
    })
}

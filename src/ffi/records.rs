use crate::error::KvError;
use crate::ffi::{handle, parse, parse_vector};
use crate::store::{PutItem, delete_records, get_records, list_records, put_records};
use serde::Deserialize;
use serde_json::Value;
use std::ffi::c_char;
use std::os::raw::c_int;

#[derive(Debug, Deserialize)]
struct GetReq {
    collection: String,
    #[serde(default)]
    key: Option<String>,
    #[serde(default)]
    keys: Option<Vec<String>>,
    #[serde(default)]
    with_vector: bool,
}

#[derive(Debug, Deserialize)]
struct PutItemReq {
    key: String,
    #[serde(default)]
    value: Option<String>,
    #[serde(default)]
    value_type: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    text_hash: Option<String>,
    #[serde(default)]
    meta: Option<Value>,
    #[serde(default)]
    vector: Option<Value>,
    #[serde(default)]
    dim: Option<i64>,
    #[serde(default)]
    force: bool,
    #[serde(default)]
    if_version: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct PutReq {
    collection: String,
    items: Vec<PutItemReq>,
}

#[derive(Debug, Deserialize)]
struct DeleteReq {
    collection: String,
    keys: Vec<String>,
    #[serde(default)]
    delete_vector_only: bool,
}

#[derive(Debug, Deserialize)]
struct ListReq {
    collection: String,
    #[serde(default)]
    prefix: Option<String>,
    #[serde(default)]
    page: Option<usize>,
    #[serde(default)]
    page_size: Option<usize>,
    #[serde(default)]
    with_value: bool,
    #[serde(default)]
    with_vector: bool,
}

#[unsafe(no_mangle)]
pub extern "C" fn post_kv_get(json_in: *const c_char, json_out: *mut c_char) -> c_int {
    handle(json_in, json_out, |store, body| {
        let req: GetReq = parse(body)?;
        let mut keys = req.keys.unwrap_or_default();
        if let Some(key) = req.key {
            keys.push(key);
        }
        if keys.is_empty() {
            return Err(KvError::bad_request("key or keys is required"));
        }
        let result = get_records(store, &req.collection, &keys, req.with_vector)?;
        Ok(serde_json::to_value(result).unwrap_or(Value::Null))
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn post_kv_put(json_in: *const c_char, json_out: *mut c_char) -> c_int {
    handle(json_in, json_out, |store, body| {
        let req: PutReq = parse(body)?;
        let mut items = Vec::with_capacity(req.items.len());
        for item in req.items {
            let vector = match &item.vector {
                Some(value) => Some(parse_vector(value)?),
                None => None,
            };
            items.push(PutItem {
                key: item.key,
                value: item.value,
                value_type: item.value_type,
                text: item.text,
                text_hash: item.text_hash,
                meta: item.meta,
                vector,
                dim: item.dim,
                force: item.force,
                if_version: item.if_version,
            });
        }
        let result = put_records(store, &req.collection, &items)?;
        Ok(serde_json::to_value(result).unwrap_or(Value::Null))
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn post_kv_delete(json_in: *const c_char, json_out: *mut c_char) -> c_int {
    handle(json_in, json_out, |store, body| {
        let req: DeleteReq = parse(body)?;
        let result = delete_records(store, &req.collection, &req.keys, req.delete_vector_only)?;
        Ok(serde_json::to_value(result).unwrap_or(Value::Null))
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn post_kv_list(json_in: *const c_char, json_out: *mut c_char) -> c_int {
    handle(json_in, json_out, |store, body| {
        let req: ListReq = parse(body)?;
        let result = list_records(
            store,
            &req.collection,
            req.prefix.as_deref(),
            req.page.unwrap_or(1),
            req.page_size.unwrap_or(20),
            req.with_value,
            req.with_vector,
        )?;
        Ok(serde_json::to_value(result).unwrap_or(Value::Null))
    })
}

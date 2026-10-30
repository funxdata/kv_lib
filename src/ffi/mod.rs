mod collections;
mod records;
mod stats;
mod vectors;

use crate::db::FfiDb;
use crate::embedding::default_embedder;
use crate::error::{KvError, Result};
use crate::store::Store;
use crate::utils::cjson::{write_error, write_success};
use crate::utils::hex;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::ffi::{CStr, c_char, c_int};
use std::sync::{Arc, OnceLock};

fn store() -> &'static Store {
    static STORE: OnceLock<Store> = OnceLock::new();
    STORE.get_or_init(|| Store::new(Arc::new(FfiDb::from_env()), default_embedder()))
}

fn read_body(json_in: *const c_char) -> std::result::Result<String, i32> {
    if json_in.is_null() {
        return Err(400100);
    }
    let body = unsafe { CStr::from_ptr(json_in) }
        .to_str()
        .map_err(|_| 400101)?;
    if body.trim().is_empty() {
        Ok("{}".to_string())
    } else {
        Ok(body.to_string())
    }
}

fn parse<T: DeserializeOwned>(body: &str) -> Result<T> {
    serde_json::from_str(body).map_err(|err| KvError::new(400102, format!("invalid json: {err}")))
}

fn handle<F>(json_in: *const c_char, json_out: *mut c_char, f: F) -> c_int
where
    F: FnOnce(&Store, &str) -> Result<Value>,
{
    let body = match read_body(json_in) {
        Ok(body) => body,
        Err(code) => {
            let msg = if code == 400100 {
                "json_in is null"
            } else {
                "invalid utf-8"
            };
            return write_error(json_out, code, msg);
        }
    };
    match f(store(), &body) {
        Ok(data) => write_success(json_out, data),
        Err(err) => write_error(json_out, err.code, &err.msg),
    }
}

pub(crate) fn parse_vector(value: &Value) -> Result<Vec<f32>> {
    match value {
        Value::String(text) => hex::decode(text),
        Value::Array(items) => {
            let mut vector = Vec::with_capacity(items.len());
            for item in items {
                let number = item
                    .as_f64()
                    .ok_or_else(|| KvError::invalid_vector("vector array must contain numbers"))?;
                vector.push(number as f32);
            }
            if vector.is_empty() {
                return Err(KvError::invalid_vector("vector must not be empty"));
            }
            Ok(vector)
        }
        _ => Err(KvError::invalid_vector(
            "vector must be a hex string or an array of numbers",
        )),
    }
}

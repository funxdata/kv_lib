use crate::ffi::{handle, parse};
use crate::store::stats as store_stats;
use serde::Deserialize;
use std::ffi::c_char;
use std::os::raw::c_int;

#[derive(Debug, Deserialize, Default)]
struct StatsReq {
    #[serde(default)]
    collection: Option<String>,
}

#[unsafe(no_mangle)]
pub extern "C" fn post_kv_stats(json_in: *const c_char, json_out: *mut c_char) -> c_int {
    handle(json_in, json_out, |store, body| {
        let req: StatsReq = parse(body)?;
        store_stats(store, req.collection.as_deref())
    })
}

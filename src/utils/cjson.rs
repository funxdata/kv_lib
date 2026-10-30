use serde_json::{Value, json};
use std::ffi::{c_char, c_int};

/// Copy a JSON response into the caller-provided 64 KiB buffer.
pub fn write_json_out(json_out: *mut c_char, value: Value) -> c_int {
    let text = value.to_string();
    let bytes = text.as_bytes();
    unsafe {
        if json_out.is_null() {
            return -1;
        }
        if bytes.len() >= 65536 {
            return -2;
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), json_out as *mut u8, bytes.len());
        *json_out.add(bytes.len()) = 0;
    }
    0
}

pub fn write_error(json_out: *mut c_char, code: i32, msg: &str) -> c_int {
    write_json_out(json_out, json!({ "code": code, "data": {}, "msg": msg }))
}

pub fn write_success(json_out: *mut c_char, data: Value) -> c_int {
    write_json_out(
        json_out,
        json!({ "code": 200, "data": data, "msg": "success" }),
    )
}

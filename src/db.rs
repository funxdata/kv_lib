use anyhow::{Context, Result as AnyResult};
use libloading::Library;
use serde_json::{Value, json};
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int};
use std::sync::OnceLock;

/// Output buffer size mandated by cyctron / database.dll.
pub const OUT_BUF_SIZE: usize = 65536;

#[cfg(windows)]
pub const DEFAULT_DB_DLL: &str = "./libary/database.dll";
#[cfg(not(windows))]
pub const DEFAULT_DB_DLL: &str = "./libary/database.so";

type DbCallFn = unsafe extern "C" fn(*const c_char, *mut c_char) -> c_int;

/// Storage backend. Production uses `database.dll`; tests use a small SQLite
/// adapter so the store logic can be exercised without a running host.
pub trait Db: Send + Sync {
    fn query(&self, name: &str, sql: &str) -> AnyResult<Vec<Value>>;
    fn execute(&self, name: &str, sql: &str) -> AnyResult<()>;
}

/// `database.dll` adapter. The library is loaded lazily on first use so the
/// FFI entry points do not fail at load time.
pub struct FfiDb {
    path: String,
    lib: OnceLock<Library>,
    query_fn: OnceLock<DbCallFn>,
    execute_fn: OnceLock<DbCallFn>,
}

impl FfiDb {
    pub fn new(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            lib: OnceLock::new(),
            query_fn: OnceLock::new(),
            execute_fn: OnceLock::new(),
        }
    }

    pub fn from_env() -> Self {
        let path = std::env::var("KV_DB_DLL")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_DB_DLL.to_string());
        Self::new(path)
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    fn library(&self) -> AnyResult<&Library> {
        if let Some(lib) = self.lib.get() {
            return Ok(lib);
        }
        let lib = unsafe { Library::new(&self.path) }
            .with_context(|| format!("failed to load {}", self.path))?;
        let _ = self.lib.set(lib);
        self.lib.get().context("database library not initialized")
    }

    fn query_fn(&self) -> AnyResult<DbCallFn> {
        if let Some(f) = self.query_fn.get() {
            return Ok(*f);
        }
        let lib = self.library()?;
        let f = unsafe {
            *lib.get::<DbCallFn>(b"post_db_query")
                .context("dlsym post_db_query failed")?
        };
        let _ = self.query_fn.set(f);
        Ok(f)
    }

    fn execute_fn(&self) -> AnyResult<DbCallFn> {
        if let Some(f) = self.execute_fn.get() {
            return Ok(*f);
        }
        let lib = self.library()?;
        let f = unsafe {
            *lib.get::<DbCallFn>(b"post_db_insert")
                .context("dlsym post_db_insert failed")?
        };
        let _ = self.execute_fn.set(f);
        Ok(f)
    }

    fn call(&self, f: DbCallFn, name: &str, sql: &str) -> AnyResult<Value> {
        let request = json!({ "path": name, "sql": sql });
        let c_request = CString::new(request.to_string()).context("request contains NUL")?;
        let mut buffer = vec![0u8; OUT_BUF_SIZE];
        let ret = unsafe { f(c_request.as_ptr(), buffer.as_mut_ptr() as *mut c_char) };
        if ret != 0 {
            anyhow::bail!("database call failed with code {ret}");
        }
        let text = unsafe { CStr::from_ptr(buffer.as_ptr() as *const c_char) }
            .to_str()
            .context("database response is not UTF-8")?;
        let value: Value = serde_json::from_str(text)
            .with_context(|| format!("database response is not JSON: {text}"))?;
        let code = value.get("code").and_then(Value::as_i64).unwrap_or(-1);
        if code != 200 && code != 0 {
            let msg = value
                .get("msg")
                .and_then(Value::as_str)
                .unwrap_or("unknown error");
            anyhow::bail!("database error (code={code}): {msg}");
        }
        Ok(value)
    }
}

impl Db for FfiDb {
    fn query(&self, name: &str, sql: &str) -> AnyResult<Vec<Value>> {
        let value = self.call(self.query_fn()?, name, sql)?;
        Ok(extract_rows(&value))
    }

    fn execute(&self, name: &str, sql: &str) -> AnyResult<()> {
        self.call(self.execute_fn()?, name, sql)?;
        Ok(())
    }
}

pub fn extract_rows(value: &Value) -> Vec<Value> {
    if let Some(rows) = value.get("data").and_then(Value::as_array) {
        return rows.clone();
    }
    for key in ["rows", "list"] {
        if let Some(rows) = value
            .get("data")
            .and_then(|data| data.get(key))
            .and_then(Value::as_array)
        {
            return rows.clone();
        }
        if let Some(rows) = value.get(key).and_then(Value::as_array) {
            return rows.clone();
        }
    }
    if value.get("data").is_some_and(Value::is_object) {
        return vec![value["data"].clone()];
    }
    Vec::new()
}

/// SQL string literal with single quotes doubled.
pub fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

pub fn quote_opt(value: Option<&str>) -> String {
    match value {
        Some(value) => quote(value),
        None => "NULL".to_string(),
    }
}

pub fn in_list(values: &[String]) -> String {
    values
        .iter()
        .map(|value| quote(value))
        .collect::<Vec<_>>()
        .join(",")
}

pub fn as_i64(value: &Value, key: &str) -> i64 {
    match value.get(key) {
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0),
        Some(Value::String(s)) => s.parse().unwrap_or(0),
        _ => 0,
    }
}

pub fn as_string(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

pub fn as_optional_string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(ToString::to_string)
}

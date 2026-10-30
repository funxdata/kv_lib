use std::fmt;

/// Business error carried through the FFI boundary.
#[derive(Debug, Clone)]
pub struct KvError {
    pub code: i32,
    pub msg: String,
}

impl KvError {
    pub fn new(code: i32, msg: impl Into<String>) -> Self {
        Self {
            code,
            msg: msg.into(),
        }
    }

    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self::new(400102, msg)
    }

    pub fn unknown_collection(code: &str) -> Self {
        Self::new(400600, format!("unknown collection: {code}"))
    }

    pub fn invalid_key(key: &str) -> Self {
        Self::new(400601, format!("invalid key: {key}"))
    }

    pub fn invalid_vector(msg: impl Into<String>) -> Self {
        Self::new(400602, msg)
    }

    pub fn invalid_search(msg: impl Into<String>) -> Self {
        Self::new(400603, msg)
    }

    pub fn invalid_collection(msg: impl Into<String>) -> Self {
        Self::new(400604, msg)
    }

    pub fn readonly_collection(code: &str) -> Self {
        Self::new(
            400605,
            format!("collection is readonly or disabled: {code}"),
        )
    }

    pub fn value_too_large(msg: impl Into<String>) -> Self {
        Self::new(400606, msg)
    }

    pub fn not_found(code: &str) -> Self {
        Self::new(404100, format!("collection not found: {code}"))
    }

    pub fn conflict(code: &str) -> Self {
        Self::new(409100, format!("collection already exists: {code}"))
    }

    pub fn version_conflict(key: &str) -> Self {
        Self::new(409101, format!("version conflict: {key}"))
    }

    pub fn embedding(msg: impl Into<String>) -> Self {
        Self::new(500600, msg)
    }

    pub fn database(msg: impl Into<String>) -> Self {
        Self::new(500601, msg)
    }

    pub fn shard(msg: impl Into<String>) -> Self {
        Self::new(500602, msg)
    }

    pub fn index(msg: impl Into<String>) -> Self {
        Self::new(500603, msg)
    }

    pub fn output_too_large() -> Self {
        Self::new(500900, "response exceeds 64 KiB")
    }
}

impl fmt::Display for KvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.code, self.msg)
    }
}

impl std::error::Error for KvError {}

pub type Result<T> = std::result::Result<T, KvError>;

impl From<anyhow::Error> for KvError {
    fn from(value: anyhow::Error) -> Self {
        Self::database(format!("{value:#}"))
    }
}

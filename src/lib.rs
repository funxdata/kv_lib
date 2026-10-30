//! kv_lib — generic key-value store with vector search.
//!
//! The public surface is the `post_kv_*` C ABI; `store` contains the
//! backend-agnostic implementation and is also usable from Rust tests.

pub mod db;
pub mod embedding;
pub mod error;
pub mod ffi;
pub mod prompt;
pub mod search;
pub mod store;
pub mod utils;

pub use error::{KvError, Result};
pub use store::Store;

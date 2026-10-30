use anyhow::Result as AnyResult;
use kv_lib::db::Db;
use kv_lib::embedding::Embedder;
use rusqlite::Connection;
use rusqlite::types::ValueRef;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Mutex;

/// SQLite-backed Db used only by tests. It mirrors the JSON row shape of
/// database.dll closely enough to exercise the production store code.
pub struct TestDb {
    connections: Mutex<HashMap<String, Connection>>,
}

impl TestDb {
    pub fn new() -> Self {
        Self {
            connections: Mutex::new(HashMap::new()),
        }
    }

    fn with_connection<T>(
        &self,
        name: &str,
        f: impl FnOnce(&Connection) -> AnyResult<T>,
    ) -> AnyResult<T> {
        let mut connections = self.connections.lock().expect("test db lock");
        let conn = connections
            .entry(name.to_string())
            .or_insert_with(|| Connection::open_in_memory().expect("open in-memory sqlite"));
        f(conn)
    }
}

impl Db for TestDb {
    fn query(&self, name: &str, sql: &str) -> AnyResult<Vec<Value>> {
        self.with_connection(name, |conn| {
            let mut statement = conn.prepare(sql)?;
            let columns: Vec<String> = (0..statement.column_count())
                .map(|index| statement.column_name(index).unwrap_or("col").to_string())
                .collect();
            let mut rows = statement.query([])?;
            let mut result = Vec::new();
            while let Some(row) = rows.next()? {
                let mut object = serde_json::Map::new();
                for (index, column) in columns.iter().enumerate() {
                    let value = match row.get_ref(index)? {
                        ValueRef::Null => Value::Null,
                        ValueRef::Integer(value) => json!(value),
                        ValueRef::Real(value) => json!(value),
                        ValueRef::Text(value) => json!(String::from_utf8_lossy(value)),
                        ValueRef::Blob(value) => json!(value.to_vec()),
                    };
                    object.insert(column.clone(), value);
                }
                result.push(Value::Object(object));
            }
            Ok(result)
        })
    }

    fn execute(&self, name: &str, sql: &str) -> AnyResult<()> {
        self.with_connection(name, |conn| {
            conn.execute(sql, [])?;
            Ok(())
        })
    }
}

/// Deterministic text encoder; the dimension is configurable so store tests
/// can stay small while the smoke test exercises a realistic width.
pub struct TestEmbedder {
    dim: usize,
}

impl TestEmbedder {
    pub fn new(dim: usize) -> Self {
        assert!(dim > 0, "embedding dimension must be positive");
        Self { dim }
    }
}

impl Embedder for TestEmbedder {
    fn model(&self) -> &str {
        "test-embed"
    }

    fn dim(&self) -> usize {
        self.dim
    }

    fn embed(&self, text: &str) -> AnyResult<Vec<f32>> {
        let mut vector = vec![0.0f32; self.dim];
        for (index, byte) in text.bytes().enumerate() {
            vector[index % self.dim] += byte as f32;
        }
        if vector.iter().all(|value| *value == 0.0) {
            vector[0] = 1.0;
        }
        Ok(vector)
    }
}

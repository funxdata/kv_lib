mod common;

use common::TestDb;
use kv_lib::Store;
use kv_lib::embedding::default_embedder;
use kv_lib::search::search;
use kv_lib::store::{
    CollectionInput, PutItem, delete_collection, list_collections, put_collection, put_records,
    stats,
};
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

const DIM: i64 = 1024;
const COLLECTION: &str = "qwen-smoke";

fn millis(duration: Duration) -> f64 {
    (duration.as_secs_f64() * 1000.0 * 100.0).round() / 100.0
}

/// Real-model smoke test: qwen3-embed text → vector, then turbovec search.
///
/// The model is expected at `models/qwen3-embed/qwen3-embedding-0.6.gguf`
/// (or `KV_EMBED_MODEL`). Run explicitly:
///   cargo test --test qwen_smoke -- --ignored
#[test]
#[ignore = "requires qwen3-embed GGUF model"]
fn qwen_embedding_and_turbovec_smoke() {
    let total_started = Instant::now();
    let mut stages: Vec<(String, f64)> = Vec::new();
    let store = Store::new(Arc::new(TestDb::new()), default_embedder());

    let started = Instant::now();
    let collections = list_collections(&store).expect("seed schema");
    stages.push(("schema_seed".into(), millis(started.elapsed())));
    assert_eq!(collections.len(), 4);

    let started = Instant::now();
    put_collection(
        &store,
        CollectionInput {
            code: COLLECTION.into(),
            dim: Some(DIM),
            model: Some("qwen3-embed".into()),
            index_min_rows: Some(0),
            shard_max_mb: Some(0),
            ..Default::default()
        },
    )
    .expect("create collection");
    stages.push(("create_collection".into(), millis(started.elapsed())));

    let started = Instant::now();
    let put = put_records(
        &store,
        COLLECTION,
        &[
            PutItem {
                key: "pdf".into(),
                value: Some("导出 PDF 文档".into()),
                text: Some("导出 PDF 文档，保持页面尺寸和出血".into()),
                ..Default::default()
            },
            PutItem {
                key: "bg".into(),
                value: Some("蓝色科技背景".into()),
                text: Some("蓝色科技背景素材，深蓝渐变".into()),
                ..Default::default()
            },
            PutItem {
                key: "layer".into(),
                value: Some("整理图层".into()),
                text: Some("批量重命名图层并整理页面".into()),
                ..Default::default()
            },
        ],
    )
    .expect("qwen embed + put");
    stages.push(("qwen_embed_put_3".into(), millis(started.elapsed())));
    assert_eq!(put.written, 3);
    assert_eq!(put.failed, 0);
    assert!(put.errors.is_empty(), "{:?}", put.errors);
    assert_eq!(put.model, "qwen3-embed");

    let started = Instant::now();
    let query = store.embedder().embed("导出 PDF").expect("embed query");
    stages.push(("qwen_embed_query".into(), millis(started.elapsed())));
    assert_eq!(query.len(), DIM as usize);

    let started = Instant::now();
    let hits = search(&store, COLLECTION, query.clone(), 3, 0.0, None).expect("turbovec search");
    stages.push(("turbovec_search_cold".into(), millis(started.elapsed())));
    assert_eq!(
        hits.first().map(|hit| hit.key.as_str()),
        Some("pdf"),
        "{hits:?}"
    );
    assert!(hits[0].score > hits[1].score, "{hits:?}");

    let started = Instant::now();
    let warm = search(&store, COLLECTION, query, 3, 0.0, None).expect("turbovec warm search");
    stages.push(("turbovec_search_warm".into(), millis(started.elapsed())));
    assert_eq!(warm.first().map(|hit| hit.key.as_str()), Some("pdf"));
    assert_eq!(warm[0].key, hits[0].key);

    let started = Instant::now();
    let stats = stats(&store, Some(COLLECTION)).expect("stats");
    stages.push(("stats".into(), millis(started.elapsed())));
    assert_eq!(stats["collections"][0]["index"].as_str(), Some("fresh"));
    assert_eq!(stats["collections"][0]["vectors"].as_i64(), Some(3));

    let total_ms = millis(total_started.elapsed());
    let report = json!({
        "model": "qwen3-embed",
        "dim": DIM,
        "records": 3,
        "total_ms": total_ms,
        "stages": stages
            .iter()
            .map(|(stage, ms)| json!({ "stage": stage, "ms": ms }))
            .collect::<Vec<_>>(),
        "top": hits.first().map(|hit| json!({ "key": hit.key, "score": hit.score })),
    });
    let output = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("qwen_smoke_timing.json");
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent).expect("create target dir");
    }
    std::fs::write(
        &output,
        serde_json::to_string_pretty(&report).expect("serialize report"),
    )
    .expect("write report");

    delete_collection(&store, COLLECTION, true).expect("cleanup");
    println!("qwen smoke total: {total_ms:.2} ms");
    for (stage, ms) in &stages {
        println!("  {stage:<20} {ms:>10.2} ms");
    }
    println!("timing report: {}", output.display());
    assert!(output.exists());
    assert!(total_ms > 0.0);
}

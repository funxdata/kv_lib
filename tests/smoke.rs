mod common;

use common::{TestDb, TestEmbedder};
use kv_lib::Store;
use kv_lib::search::search;
use kv_lib::store::{
    CollectionInput, PutItem, delete_collection, delete_records, embed_records, get_records,
    list_collections, put_collection, put_records, rebuild, stats,
};
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const DIM: usize = 128;
const RECORDS: usize = 1000;
const BATCH: usize = 500;

fn millis(duration: Duration) -> f64 {
    (duration.as_secs_f64() * 1000.0 * 100.0).round() / 100.0
}

fn timed<T>(stages: &mut Vec<(String, f64)>, name: &str, f: impl FnOnce() -> T) -> T {
    let started = Instant::now();
    let result = f();
    stages.push((name.to_string(), millis(started.elapsed())));
    result
}

/// End-to-end smoke test: cold schema, collection, 1k writes, retrieval,
/// vector repair, stats, rebuild and cleanup. Every stage is timed and the
/// report is written to `target/kv_smoke_timing.json`.
///
/// Run with:
///   cargo test --test smoke -- --nocapture
#[test]
fn kv_end_to_end_smoke_with_timing() {
    let total_started = Instant::now();
    let mut stages: Vec<(String, f64)> = Vec::new();
    let store = Store::new(Arc::new(TestDb::new()), Arc::new(TestEmbedder::new(DIM)));

    let collections =
        timed(&mut stages, "schema_seed", || list_collections(&store)).expect("seed schema");
    assert_eq!(collections.len(), 4);

    timed(&mut stages, "create_collection", || {
        put_collection(
            &store,
            CollectionInput {
                code: "smoke".into(),
                dim: Some(DIM as i64),
                model: Some("test-embed".into()),
                index_min_rows: Some(1),
                shard_max_mb: Some(0),
                ..Default::default()
            },
        )
    })
    .expect("create collection");

    let items: Vec<PutItem> = (0..RECORDS)
        .map(|index| PutItem {
            key: format!("doc-{index:04}"),
            value: Some(format!("document {index}")),
            text: Some(format!("document {index} 导出 pdf 设计 素材 插件 当前文档")),
            ..Default::default()
        })
        .collect();

    let (written, failed) = timed(&mut stages, "put_1000", || {
        let mut written = 0usize;
        let mut failed = 0usize;
        for chunk in items.chunks(BATCH) {
            let result = put_records(&store, "smoke", chunk).expect("put batch");
            written += result.written;
            failed += result.failed;
        }
        (written, failed)
    });
    assert_eq!(written, RECORDS);
    assert_eq!(failed, 0);

    let keys: Vec<String> = (0..10).map(|index| format!("doc-{index:04}")).collect();
    let fetched = timed(&mut stages, "get_10", || {
        get_records(&store, "smoke", &keys, false)
    })
    .expect("get records");
    assert_eq!(fetched.list.len(), 10);

    let query = store
        .embedder()
        .embed("document 42 导出 pdf 设计")
        .expect("embed query");
    let cold = timed(&mut stages, "search_cold", || {
        search(&store, "smoke", query.clone(), 10, 0.0, None)
    })
    .expect("cold search");
    assert_eq!(cold.len(), 10);

    let query = store
        .embedder()
        .embed("document 42 导出 pdf 设计")
        .expect("embed query");
    let warm = timed(&mut stages, "search_warm", || {
        search(&store, "smoke", query, 10, 0.0, None)
    })
    .expect("warm search");
    assert_eq!(warm.len(), 10);
    assert_eq!(cold[0].key, warm[0].key, "cache must be stable");

    let half_keys: Vec<String> = (0..RECORDS / 2)
        .map(|index| format!("doc-{index:04}"))
        .collect();
    let cleared = timed(&mut stages, "delete_vector_500", || {
        delete_records(&store, "smoke", &half_keys, true)
    })
    .expect("clear vectors");
    assert_eq!(cleared.deleted, RECORDS / 2);

    let repaired = timed(&mut stages, "embed_repair_500", || {
        embed_records(&store, "smoke", None, None, None, false)
    })
    .expect("repair vectors");
    assert_eq!(repaired.processed, RECORDS / 2);
    assert_eq!(repaired.failed, 0);
    assert_eq!(repaired.skipped, RECORDS / 2);

    let before =
        timed(&mut stages, "stats_before", || stats(&store, Some("smoke"))).expect("stats before");
    assert_eq!(
        before["collections"][0]["rows"].as_i64(),
        Some(RECORDS as i64)
    );
    assert_eq!(
        before["collections"][0]["vectors"].as_i64(),
        Some(RECORDS as i64)
    );

    let rebuilt = timed(&mut stages, "rebuild_index", || {
        rebuild(&store, "smoke", "index", true)
    })
    .expect("rebuild index");
    assert_eq!(rebuilt["rebuilt"].as_bool(), Some(true));

    let quarter: Vec<String> = (0..RECORDS / 4)
        .map(|index| format!("doc-{index:04}"))
        .collect();
    let deleted = timed(&mut stages, "delete_250", || {
        delete_records(&store, "smoke", &quarter, false)
    })
    .expect("delete records");
    assert_eq!(deleted.deleted, RECORDS / 4);

    let after =
        timed(&mut stages, "stats_after", || stats(&store, Some("smoke"))).expect("stats after");
    assert_eq!(
        after["collections"][0]["rows"].as_i64(),
        Some((RECORDS - RECORDS / 4) as i64)
    );

    let (shards, records) = timed(&mut stages, "delete_collection", || {
        delete_collection(&store, "smoke", true)
    })
    .expect("delete collection");
    assert_eq!(shards, 1);
    assert_eq!(records, (RECORDS - RECORDS / 4) as i64);

    let total_ms = millis(total_started.elapsed());
    let started_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);

    let report = json!({
        "dim": DIM,
        "records": RECORDS,
        "started_at_unix": started_at,
        "total_ms": total_ms,
        "stages": stages
            .iter()
            .map(|(stage, ms)| json!({ "stage": stage, "ms": ms }))
            .collect::<Vec<_>>(),
        "search_top": cold.first().map(|hit| json!({
            "key": hit.key,
            "score": hit.score,
        })),
    });

    let output = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("kv_smoke_timing.json");
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent).expect("create target dir");
    }
    std::fs::write(
        &output,
        serde_json::to_string_pretty(&report).expect("serialize report"),
    )
    .expect("write timing report");

    println!("kv smoke total: {total_ms:.2} ms (dim={DIM}, records={RECORDS})");
    for (stage, ms) in &stages {
        println!("  {stage:<22} {ms:>10.2} ms");
    }
    println!("timing report: {}", output.display());

    assert!(output.exists(), "timing report was not written");
    assert!(total_ms > 0.0, "timing must be positive");
}

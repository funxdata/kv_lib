mod common;

use common::{TestDb, TestEmbedder};
use kv_lib::Store;
use kv_lib::prompt;
use kv_lib::search::search;
use kv_lib::store::{
    CollectionInput, PutItem, delete_collection, delete_records, get_records, list_collections,
    list_records, put_collection, put_records, rebuild, stats,
};
use std::sync::Arc;

fn store() -> Store {
    Store::new(Arc::new(TestDb::new()), Arc::new(TestEmbedder::new(4)))
}

fn create_collection(store: &Store, code: &str, index_min_rows: i64) {
    put_collection(
        store,
        CollectionInput {
            code: code.to_string(),
            dim: Some(4),
            model: Some("test-embed".to_string()),
            index_min_rows: Some(index_min_rows),
            ..Default::default()
        },
    )
    .expect("create collection");
}

fn item(key: &str, value: &str, text: &str) -> PutItem {
    PutItem {
        key: key.to_string(),
        value: Some(value.to_string()),
        text: Some(text.to_string()),
        ..Default::default()
    }
}

#[test]
fn seeds_collections_and_prompt() {
    let store = store();
    let collections = list_collections(&store).expect("list collections");
    let codes: Vec<&str> = collections
        .iter()
        .map(|info| info.collection.code.as_str())
        .collect();
    assert_eq!(codes, vec!["prompt", "plugin", "skill", "material"]);

    let (base, template, rules, limits) = prompt::load_defaults(&store).expect("prompt defaults");
    assert!(base.contains("有用的助手"));
    assert!(template.contains("{{skills}}"));
    assert_eq!(rules.len(), 4);
    assert_eq!(limits.max_skills, 20);
    assert_eq!(limits.max_system_chars, 6000);
}

#[test]
fn put_get_list_delete_records() {
    let store = store();
    create_collection(&store, "demo", 0);

    let result = put_records(
        &store,
        "demo",
        &[item("a", "alpha", "alpha"), item("b", "beta", "beta")],
    )
    .expect("put");
    assert_eq!(result.written, 2);
    assert_eq!(result.skipped, 0);
    assert_eq!(result.failed, 0);

    let fetched = get_records(&store, "demo", &["a".into(), "missing".into()], false).expect("get");
    assert_eq!(fetched.list.len(), 1);
    assert_eq!(fetched.missing, vec!["missing"]);

    let listed = list_records(&store, "demo", Some("a"), 1, 20, true, false).expect("list");
    assert_eq!(listed.total, 1);
    assert_eq!(listed.list[0]["key"], "a");
    assert_eq!(listed.list[0]["value"], "alpha");

    let deleted = delete_records(&store, "demo", &["a".to_string()], false).expect("delete");
    assert_eq!(deleted.deleted, 1);
    assert!(deleted.missing.is_empty());

    let after = get_records(&store, "demo", &["a".to_string()], false).expect("get after delete");
    assert_eq!(after.list.len(), 0);
    assert_eq!(after.missing, vec!["a"]);
}

#[test]
fn embeds_and_searches_with_cache() {
    let store = store();
    create_collection(&store, "demo", 1);
    put_records(
        &store,
        "demo",
        &[
            item("alpha", "alpha", "alpha"),
            item("beta", "beta", "beta"),
        ],
    )
    .expect("put");

    let query = store.embedder().embed("alpha").expect("embed query");
    let hits = search(&store, "demo", query, 2, 0.0, None).expect("search");
    assert_eq!(hits.first().map(|hit| hit.key.as_str()), Some("alpha"));

    let query = store.embedder().embed("alpha").expect("embed query");
    let hits = search(&store, "demo", query, 5, 0.0, Some(&["beta".to_string()]))
        .expect("search candidate");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].key, "beta");

    let query = store.embedder().embed("alpha").expect("embed query");
    let hits = search(&store, "demo", query, 5, 0.99, None).expect("search min_score");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].key, "alpha");
}

#[test]
fn embed_rebuilds_missing_vectors() {
    let store = store();
    create_collection(&store, "demo", 0);
    put_records(&store, "demo", &[item("alpha", "alpha", "alpha")]).expect("put");
    delete_records(&store, "demo", &["alpha".to_string()], true).expect("clear vector");

    let embedded = kv_lib::store::embed_records(&store, "demo", None, None, None, false)
        .expect("embed records");
    assert_eq!(embedded.processed, 1);
    assert_eq!(embedded.failed, 0);

    let query = store.embedder().embed("alpha").expect("embed query");
    let hits = search(&store, "demo", query, 1, 0.0, None).expect("search");
    assert_eq!(hits[0].key, "alpha");
}

#[test]
fn version_conflict_is_reported() {
    let store = store();
    create_collection(&store, "demo", 0);
    put_records(&store, "demo", &[item("a", "alpha", "alpha")]).expect("put");

    let mut update = item("a", "beta", "beta");
    update.if_version = Some(99);
    let result = put_records(&store, "demo", &[update]).expect("batch result");
    assert_eq!(result.failed, 1);
    assert_eq!(result.errors[0].code, 409101);
}

#[test]
fn non_empty_collection_requires_hard_delete() {
    let store = store();
    create_collection(&store, "demo", 0);
    put_records(&store, "demo", &[item("a", "alpha", "alpha")]).expect("put");

    let err = delete_collection(&store, "demo", false).expect_err("must refuse");
    assert_eq!(err.code, 400605);
    let (shards, records) = delete_collection(&store, "demo", true).expect("hard delete");
    assert_eq!(shards, 1);
    assert_eq!(records, 1);
}

#[test]
fn stats_and_rebuild_cover_all_actions() {
    let store = store();
    create_collection(&store, "demo", 0);
    put_records(&store, "demo", &[item("a", "alpha", "alpha")]).expect("put");

    let value = stats(&store, Some("demo")).expect("stats");
    assert_eq!(value["collections"][0]["rows"], 1);
    assert_eq!(value["collections"][0]["vectors"], 1);

    assert!(
        rebuild(&store, "demo", "index", true).expect("rebuild index")["rebuilt"]
            .as_bool()
            .unwrap()
    );
    assert_eq!(
        rebuild(&store, "demo", "shards", false).expect("rebuild shards")["processed"],
        1
    );
    assert_eq!(
        rebuild(&store, "demo", "text_hash", false).expect("rebuild hash")["processed"],
        1
    );
    let err = rebuild(&store, "demo", "unknown", false).expect_err("bad action");
    assert_eq!(err.code, 400603);
}

#[test]
fn validates_keys_values_and_vectors() {
    let store = store();
    create_collection(&store, "demo", 0);

    let result = put_records(
        &store,
        "demo",
        &[PutItem {
            key: "bad key".into(),
            ..Default::default()
        }],
    )
    .expect("batch result");
    assert_eq!(result.failed, 1);
    assert_eq!(result.errors[0].code, 400601);

    let result = put_records(
        &store,
        "demo",
        &[PutItem {
            key: "json".into(),
            value: Some("{not json}".into()),
            value_type: Some("json".into()),
            ..Default::default()
        }],
    )
    .expect("batch result");
    assert_eq!(result.failed, 1);

    let result = put_records(
        &store,
        "demo",
        &[PutItem {
            key: "vector".into(),
            vector: Some(vec![1.0, 0.0]),
            ..Default::default()
        }],
    )
    .expect("batch result");
    assert_eq!(result.failed, 1);
    assert_eq!(result.errors[0].code, 400602);
}

#[test]
fn readonly_collection_rejects_writes() {
    let store = store();
    create_collection(&store, "demo", 0);
    put_collection(
        &store,
        CollectionInput {
            code: "demo".into(),
            state: Some(1),
            ..Default::default()
        },
    )
    .expect("set readonly");

    let err = put_records(&store, "demo", &[item("a", "alpha", "alpha")]).expect_err("readonly");
    assert_eq!(err.code, 400605);
}

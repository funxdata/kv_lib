use crate::db::{as_i64, as_optional_string, as_string, in_list};
use crate::error::{KvError, Result};
use crate::store::{Collection, Store, get_collection, list_shards, rank_order};
use crate::utils::hex;
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

const DEFAULT_TOP_K: usize = 5;
const MAX_TOP_K: usize = 100;
const MAX_CANDIDATES: usize = 500;
const SCAN_PAGE: usize = 5;
const TURBO_BIT_WIDTH: usize = 4;

#[derive(Debug, Clone)]
pub struct SearchHit {
    pub key: String,
    pub score: f32,
    pub rank: i64,
}

struct Cached {
    generation: i64,
    keys: Vec<String>,
    ranks: Vec<i64>,
    index: Option<turbovec::IdMapIndex>,
}

/// Per-collection turbovec index cache. Cache is invalidated by the collection
/// generation, which is bumped on every write or delete.
#[derive(Default)]
pub struct IndexCache {
    entries: Mutex<HashMap<String, Cached>>,
}

impl IndexCache {
    pub fn clear(&self, collection: &str) {
        if let Ok(mut entries) = self.entries.lock() {
            entries.remove(collection);
        }
    }

    pub fn invalidate(&self, collection: &str) {
        self.clear(collection);
    }

    pub fn cached_count(&self) -> usize {
        self.entries
            .lock()
            .map(|entries| entries.len())
            .unwrap_or(0)
    }

    pub fn is_cached(&self, collection: &str, generation: i64) -> bool {
        self.entries
            .lock()
            .ok()
            .and_then(|entries| {
                entries
                    .get(collection)
                    .map(|cached| cached.generation == generation)
            })
            .unwrap_or(false)
    }

    /// Drop and rebuild the turbovec index for a collection.
    pub fn rebuild(&self, store: &Store, collection: &Collection) -> Result<()> {
        self.clear(&collection.code);
        self.get_or_build(store, collection)
    }

    fn get_or_build(&self, store: &Store, collection: &Collection) -> Result<()> {
        {
            let entries = self
                .entries
                .lock()
                .map_err(|_| KvError::index("index cache lock poisoned"))?;
            if let Some(cached) = entries.get(&collection.code) {
                if cached.generation == collection.generation {
                    return Ok(());
                }
            }
        }

        let (keys, ranks, vectors) = load_all(store, collection)?;
        let dim = collection.dim as usize;
        if vectors.len() % dim != 0 {
            return Err(KvError::index("vector buffer is not aligned to dim"));
        }

        let index = if keys.is_empty() || dim % 8 != 0 {
            // turbovec requires dim to be a positive multiple of 8; other
            // dimensions fall back to the exact scan in `search`.
            None
        } else {
            let mut index = turbovec::IdMapIndex::new(dim, TURBO_BIT_WIDTH)
                .map_err(|err| KvError::index(format!("turbovec init failed: {err}")))?;
            let ids: Vec<u64> = (0..keys.len() as u64).collect();
            index
                .add_with_ids(&vectors, &ids)
                .map_err(|err| KvError::index(format!("turbovec build failed: {err}")))?;
            Some(index)
        };

        let cached = Cached {
            generation: collection.generation,
            keys,
            ranks,
            index,
        };
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| KvError::index("index cache lock poisoned"))?;
        entries.insert(collection.code.clone(), cached);
        Ok(())
    }

    pub fn search(
        &self,
        store: &Store,
        collection: &Collection,
        query: &[f32],
        top_k: usize,
        min_score: f32,
        candidates: Option<&HashSet<String>>,
    ) -> Result<Vec<SearchHit>> {
        if query.len() as i64 != collection.dim {
            return Err(KvError::invalid_vector(format!(
                "query dimension mismatch: expected {}, got {}",
                collection.dim,
                query.len()
            )));
        }
        let top_k = normalize_top_k(top_k);
        let rows = estimate_rows(store, collection);
        if rows == 0 {
            return Ok(Vec::new());
        }

        if collection.index_min_rows <= rows as i64 {
            if self.get_or_build(store, collection).is_ok() {
                if let Ok(entries) = self.entries.lock() {
                    if let Some(cached) = entries.get(&collection.code) {
                        if cached.generation == collection.generation && cached.index.is_some() {
                            return Ok(cached_search(cached, query, top_k, min_score, candidates));
                        }
                    }
                }
            } else {
                eprintln!(
                    "[kv_lib] turbovec build failed for {}, falling back to exact scan",
                    collection.code
                );
            }
        }

        exact_scan(store, collection, query, top_k, min_score, candidates)
    }
}

fn estimate_rows(store: &Store, collection: &Collection) -> usize {
    list_shards(store, &collection.code, false)
        .map(|shards| {
            shards
                .iter()
                .map(|shard| shard.row_count.max(0) as usize)
                .sum()
        })
        .unwrap_or(0)
}

fn normalize_top_k(top_k: usize) -> usize {
    if top_k == 0 {
        DEFAULT_TOP_K
    } else {
        top_k.min(MAX_TOP_K)
    }
}

fn cached_search(
    cached: &Cached,
    query: &[f32],
    top_k: usize,
    min_score: f32,
    candidates: Option<&HashSet<String>>,
) -> Vec<SearchHit> {
    let Some(index) = cached.index.as_ref() else {
        return Vec::new();
    };
    let (scores, ids) = index.search(query, top_k.saturating_mul(2).max(top_k));
    let mut best: HashMap<&str, (f32, i64)> = HashMap::new();
    for (score, id) in scores.iter().zip(ids.iter()) {
        let Some(key) = cached.keys.get(*id as usize) else {
            continue;
        };
        let rank = cached.ranks.get(*id as usize).copied().unwrap_or(0);
        if candidates.is_some_and(|set| !set.contains(key)) {
            continue;
        }
        if *score < min_score {
            continue;
        }
        let entry = best.entry(key.as_str()).or_insert((f32::MIN, rank));
        if *score > entry.0 {
            *entry = (*score, rank);
        }
    }
    let mut hits: Vec<SearchHit> = best
        .into_iter()
        .map(|(key, (score, rank))| SearchHit {
            key: key.to_string(),
            score,
            rank,
        })
        .collect();
    sort_hits(&mut hits);
    hits.truncate(top_k);
    hits
}

fn exact_scan(
    store: &Store,
    collection: &Collection,
    query: &[f32],
    top_k: usize,
    min_score: f32,
    candidates: Option<&HashSet<String>>,
) -> Result<Vec<SearchHit>> {
    if candidates.is_some_and(|set| set.len() > MAX_CANDIDATES) {
        return Err(KvError::invalid_search(format!(
            "at most {MAX_CANDIDATES} candidate keys per request"
        )));
    }
    let candidate_sql = candidates
        .filter(|set| !set.is_empty())
        .map(|set| {
            let keys: Vec<String> = set.iter().cloned().collect();
            format!(" AND key IN ({})", in_list(&keys))
        })
        .unwrap_or_default();

    let mut best: HashMap<String, (f32, i64)> = HashMap::new();
    for shard in list_shards(store, &collection.code, false)? {
        let mut offset = 0usize;
        loop {
            let rows = store.query(
                &shard.path,
                &format!(
                    "SELECT key, dim, rank, hex(vector) AS vector_hex FROM kv_record
                     WHERE vector IS NOT NULL{candidate_sql}
                     ORDER BY id LIMIT {SCAN_PAGE} OFFSET {offset}"
                ),
            )?;
            let count = rows.len();
            if count == 0 {
                break;
            }
            for row in &rows {
                let dim = as_i64(row, "dim");
                if dim != collection.dim {
                    continue;
                }
                let Some(text) = as_optional_string(row, "vector_hex") else {
                    continue;
                };
                let vector = match hex::decode(&text) {
                    Ok(vector) if vector.len() == query.len() => vector,
                    _ => continue,
                };
                let score = dot(query, &vector);
                if score < min_score {
                    continue;
                }
                let key = as_string(row, "key");
                let rank = as_i64(row, "rank");
                let entry = best.entry(key).or_insert((f32::MIN, rank));
                if score > entry.0 {
                    *entry = (score, rank);
                }
            }
            offset += count;
            if count < SCAN_PAGE {
                break;
            }
        }
    }

    let mut hits: Vec<SearchHit> = best
        .into_iter()
        .map(|(key, (score, rank))| SearchHit { key, score, rank })
        .collect();
    sort_hits(&mut hits);
    hits.truncate(top_k);
    Ok(hits)
}

fn load_all(store: &Store, collection: &Collection) -> Result<(Vec<String>, Vec<i64>, Vec<f32>)> {
    let mut keys = Vec::new();
    let mut ranks = Vec::new();
    let mut vectors = Vec::new();
    for shard in list_shards(store, &collection.code, false)? {
        let mut offset = 0usize;
        loop {
            let rows = store.query(
                &shard.path,
                &format!(
                    "SELECT key, dim, rank, hex(vector) AS vector_hex FROM kv_record
                     WHERE vector IS NOT NULL ORDER BY id LIMIT {SCAN_PAGE} OFFSET {offset}"
                ),
            )?;
            let count = rows.len();
            if count == 0 {
                break;
            }
            for row in &rows {
                if as_i64(row, "dim") != collection.dim {
                    continue;
                }
                let Some(text) = as_optional_string(row, "vector_hex") else {
                    continue;
                };
                let vector = match hex::decode(&text) {
                    Ok(vector) if vector.len() == collection.dim as usize => vector,
                    _ => continue,
                };
                keys.push(as_string(row, "key"));
                ranks.push(as_i64(row, "rank"));
                vectors.extend_from_slice(&vector);
            }
            offset += count;
            if count < SCAN_PAGE {
                break;
            }
        }
    }
    Ok((keys, ranks, vectors))
}

fn dot(left: &[f32], right: &[f32]) -> f32 {
    left.iter()
        .zip(right.iter())
        .map(|(a, b)| a * b)
        .sum::<f32>()
}

fn sort_hits(hits: &mut [SearchHit]) {
    hits.sort_by(|a, b| {
        rank_order(a.rank)
            .cmp(&rank_order(b.rank))
            .then_with(|| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| a.key.cmp(&b.key))
    });
}

/// Prepare a caller-supplied or model-generated vector for search.
pub fn prepare_query(vector: Vec<f32>, dim: i64) -> Result<Vec<f32>> {
    let vector = hex::normalize(vector)?;
    if vector.len() as i64 != dim {
        return Err(KvError::invalid_vector(format!(
            "query dimension mismatch: expected {dim}, got {}",
            vector.len()
        )));
    }
    Ok(vector)
}

pub fn search(
    store: &Store,
    collection_code: &str,
    query: Vec<f32>,
    top_k: usize,
    min_score: f32,
    candidate_keys: Option<&[String]>,
) -> Result<Vec<SearchHit>> {
    store.ensure_schema()?;
    let collection = get_collection(store, collection_code)?;
    if collection.state == 2 {
        return Err(KvError::readonly_collection(&collection.code));
    }
    let query = prepare_query(query, collection.dim)?;
    let candidates = candidate_keys.map(|keys| keys.iter().cloned().collect::<HashSet<String>>());
    store.index().search(
        store,
        &collection,
        &query,
        top_k,
        min_score,
        candidates.as_ref(),
    )
}

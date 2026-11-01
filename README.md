# kv_lib

Generic **key-value store with vector search** for Piksel / cyctron.
Compiled as a Rust `cdylib` and exposed as `POST /kv_lib` + `FFI-Symbol: kv_*`.

| Item | Value |
| --- | --- |
| Version | 0.1.0 |
| Crate type | `cdylib` + `rlib` |
| Artifact | `kv_lib.dll` / `libkv_lib.so` / `libkv_lib.dylib` |
| Storage | `database/kv.db` + `database/kv_<collection>_<n>.db` |
| Internal dependency | `database.dll` (SQLite) |
| Vector runtime | `llama-cpp-2` + qwen3-embed (text → vector); `turbovec` (indexed search) |

## Features

- Collections with per-collection `dim`, `model`, shard size and index threshold;
- Records keyed by `collection + key`, with `value`, `value_type`, `text`, `meta`, `rank`, `version`;
- `rank` is a business ordering field: lower values come first in `kv_list` / `kv_search`,
  and `0` means unranked (sorted last);
- `text_hash` / SHA-256 idempotency: unchanged text is skipped, changed text is re-embedded;
- Sharded storage: a collection rolls to a new shard when the active file exceeds `shard_max_mb`;
- Vector search: qwen3-embed (1024-dim, L2-normalized) + `turbovec` index cached per collection
  and invalidated by collection generation;
- Built-in `prompt` collection seeded with base prompt / template / rules / limits;
- 4 FFI symbols: `kv_collection`, `kv_record`, `kv_embed`, `kv_search`.

## Building

```bash
# qwen3-embed needs libclang; turbovec and llama.cpp need CMake / MSVC
LIBCLANG_PATH="C:\Program Files\LLVM\bin" cargo build --release
```

The default model is `./models/qwen3-embed/qwen3-embedding-0.6.gguf`; override it with
`KV_EMBED_MODEL` when the model is installed elsewhere. `kv_put` with `text` and
`kv_search` with `query` use qwen3-embed automatically.

`turbovec` requires a dimension that is a positive multiple of 8. qwen3-embed uses 1024;
collections with other dimensions fall back to the exact scan.

`database.dll` is loaded lazily from `KV_DB_DLL` or `./libary/database.dll`
(`database.so` on Linux).

## FFI

| Symbol | `action` | Purpose |
| --- | --- | --- |
| `kv_collection` | `list`（默认）/ `put` / `delete` / `stats` | 集合列表、创建更新、删除与统计 |
| `kv_record` | `list`（默认）/ `get` / `put` / `delete` | 记录读取、写入、删除与分页扫描 |
| `kv_embed` | `embed`（默认）/ `rebuild` | 向量化；`rebuild` 用 `target` 指定 `index` / `shards` / `text_hash` |
| `kv_search` | — | 按 `query` 文本或 `vector` 做相似度检索 |

请求统一为 `{"action": "...", ...}`：

```json
{"action":"list","page":1,"page_size":20}
{"action":"put","code":"plugin","name":"插件能力","dim":1024}
{"action":"delete","code":"plugin","hard":true}
{"action":"stats","collection":"plugin"}
{"action":"get","collection":"plugin","keys":["k1"],"with_vector":false}
{"action":"rebuild","collection":"plugin","target":"index","force":false}
```

Request / response shapes are documented in
[`sysinfo/sys/kv_lib/ffi.md`](https://github.com/funxdata/dytes_sys_operations/blob/main/sysinfo/sys/kv_lib/ffi.md)
and the storage design in
[`sysinfo/sys/kv_lib/db.md`](https://github.com/funxdata/dytes_sys_operations/blob/main/sysinfo/sys/kv_lib/db.md).

## Testing

```bash
cargo test
```

The test suite uses an in-memory SQLite adapter (`TestDb`) and a deterministic `TestEmbedder`,
so it needs neither `database.dll` nor a GGUF model. It covers schema seeding, collection
lifecycle, record CRUD, prompt assembly, embedding / search, cache invalidation, version
conflicts and rebuild actions.

For an end-to-end FFI check, use the real `database.dll` and point `KV_DB_DLL` at it:

```powershell
$env:KV_DB_DLL = "D:\path\to\libary\database.dll"
```

### Smoke timing

`tests/smoke.rs` runs the full KV lifecycle (schema → collection → 1,000 writes →
get → cold/warm search → vector repair → stats → rebuild → delete) with a 128-dim
deterministic embedder. Each stage is timed and the report is written to
`target/kv_smoke_timing.json`:

```bash
cargo test --test smoke -- --nocapture
```

The test also prints the per-stage table; it does not assert wall-clock thresholds,
so it stays stable on slow CI machines.

### Real-model smoke (qwen3-embed + turbovec)

`tests/qwen_smoke.rs` loads the real qwen3-embed GGUF and runs text → vector →
turbovec search, including cold and warm searches. It is ignored by default:

```bash
KV_EMBED_MODEL="D:/codes/github/funxdata/sys/kv_lib/models/qwen3-embed/qwen3-embedding-0.6.gguf" \
  cargo test --test qwen_smoke -- --ignored
```

The report is written to `target/qwen_smoke_timing.json`.

## Storage layout

```text
database/
├── kv.db                 # kv_collection / kv_shard
├── kv_prompt_0.db        # prompt collection
├── kv_plugin_0.db        # plugin capability vectors
├── kv_skill_0.db         # skill vectors
└── kv_material_0.db      # material content vectors
```

`kv.db` holds only small metadata tables; records and vectors live in per-collection shards.

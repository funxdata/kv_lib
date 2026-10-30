use anyhow::{Context, Result};

/// Run expensive work on a large stack so the host request thread stays small.
pub fn run_with_large_stack<T, F>(f: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    let handle = std::thread::Builder::new()
        .name("kv-lib-worker".to_string())
        .stack_size(64 * 1024 * 1024)
        .spawn(f)
        .context("failed to create worker thread")?;
    handle
        .join()
        .map_err(|_| anyhow::anyhow!("worker thread panicked"))?
}

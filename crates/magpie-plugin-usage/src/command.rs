//! CLI metadata query supplied by this plugin, with no network or credential access.
use magpie_core::provider_store::ProviderStore;
use serde_json::Value;
use std::path::PathBuf;

/// Registered compatibility command documentation.
pub const HELP: &str = "magpie usage --json [--limit 1..1000] [--store ABS_PATH]\nReads recent request metadata offline. Default limit: 100. Unknown tokens remain null.";

/// Executes the plugin's read-only command after the host has selected it.
///
/// # Errors
/// Rejects malformed options and unsafe metadata paths without exposing their contents.
pub fn execute(arguments: &[String]) -> Result<Value, String> {
    let mut args = arguments.iter();
    let mut store = None;
    let mut limit = None;
    let mut json = false;
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--json" if !json => json = true,
            "--limit" if limit.is_none() => {
                let n = args
                    .next()
                    .and_then(|v| v.parse::<usize>().ok())
                    .filter(|n| (1..=1000).contains(n))
                    .ok_or("usage_error: --limit must be between 1 and 1000")?;
                limit = Some(n);
            }
            "--store" if store.is_none() => {
                let path = PathBuf::from(
                    args.next()
                        .ok_or("usage_error: --store requires an absolute path")?,
                );
                if !path.is_absolute() {
                    return Err("usage_error: --store requires an absolute path".into());
                }
                store = Some(path);
            }
            _ => {
                return Err(
                    "usage_error: unknown or repeated option; try 'magpie usage --help'".into(),
                );
            }
        }
    }
    if !json {
        return Err("usage_error: use --json to query recent metadata".into());
    }
    let store = store
        .map(Ok)
        .unwrap_or_else(ProviderStore::default_path)
        .map_err(|_| "usage_error: cannot determine Provider store path")?;
    crate::report(&store, limit.unwrap_or(100)).map_err(|_| {
        "usage_error: metadata could not be read safely; no account data was accessed".into()
    })
}

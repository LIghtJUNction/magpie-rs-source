//! Typed Provider extension of the packaged CLI boundary. This module contains
//! wire objects only; magpie_core::providers remains the sole persisted truth.
use crate::cli_bridge::{self, BridgeError, Cancellation, ErrorKind};
use serde::Deserialize;
use serde_json::Value;
use std::{collections::HashSet, ffi::OsString, path::PathBuf};

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Key {
    pub id: String,
    pub name: String,
    pub env: String,
    pub enabled: bool,
    pub deleted: bool,
    pub masked: String,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Provider {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub protocol: String,
    pub enabled: bool,
    pub deleted: bool,
    pub allow_insecure_http: bool,
    pub keys: Vec<Key>,
    pub models: Vec<String>,
    pub selected_models: Vec<String>,
    pub auto_include: bool,
    pub catalog_checked_at: Option<u64>,
    pub catalog_error: Option<String>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Operation {
    pub kind: String,
    pub ok: bool,
    pub code: String,
    pub http_status: Option<u16>,
    pub elapsed_ms: u64,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Snapshot {
    pub version: String,
    pub schema_version: u32,
    pub revision: u64,
    pub token: String,
    pub providers: Vec<Provider>,
    #[serde(default)]
    pub operation: Option<Operation>,
}
impl Snapshot {
    pub fn active(&self) -> impl Iterator<Item = &Provider> {
        self.providers.iter().filter(|p| !p.deleted)
    }
    pub fn provider(&self, id: &str) -> Option<&Provider> {
        self.active().find(|p| p.id == id)
    }
}
#[derive(Clone)]
pub(crate) struct Request {
    pub command: &'static str,
    pub id: Option<String>,
    pub token: Option<String>,
    pub data: Option<Value>,
}
impl Request {
    pub fn list() -> Self {
        Self {
            command: "list",
            id: None,
            token: None,
            data: None,
        }
    }
    pub fn mutation(&self) -> bool {
        !matches!(self.command, "list" | "show" | "test")
    }
}
#[derive(Clone, Debug)]
pub(crate) struct Failure {
    pub code: String,
    pub uncertain: bool,
}
impl From<BridgeError> for Failure {
    fn from(e: BridgeError) -> Self {
        Self {
            code: e.code().to_owned(),
            uncertain: e.mutation_may_have_run,
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ErrorEnvelope {
    version: String,
    error: ErrorDetail,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ErrorDetail {
    code: String,
    message: String,
}
fn text_ok(s: &str, limit: usize) -> bool {
    !s.is_empty() && s.len() <= limit && !s.chars().any(char::is_control)
}
fn identifier(s: &str) -> bool {
    text_ok(s, 128)
        && s != "group"
        && s.as_bytes().first().is_some_and(u8::is_ascii_alphanumeric)
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}
fn env_ok(s: &str) -> bool {
    s.len() <= 128
        && s.as_bytes()
            .first()
            .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}
fn model_ok(s: &str) -> bool {
    s.len() <= 256
        && s.as_bytes().first().is_some_and(u8::is_ascii_alphanumeric)
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-:@/".contains(&b))
        && s.split('/').all(|part| !matches!(part, "" | "." | ".."))
}
fn token_ok(token: &str, revision: u64) -> bool {
    token.split_once(':').is_some_and(|(n, d)| {
        n == revision.to_string() && d.len() == 64 && d.bytes().all(|b| b.is_ascii_hexdigit())
    })
}
fn invalid(mutation: bool) -> Failure {
    Failure {
        code: "cli_reply_invalid".into(),
        uncertain: mutation,
    }
}
fn validate(snapshot: &Snapshot) -> bool {
    if snapshot.version != "0.2.0"
        || snapshot.schema_version != 1
        || !token_ok(&snapshot.token, snapshot.revision)
        || snapshot.providers.len() > 128
    {
        return false;
    }
    let mut providers = HashSet::new();
    for p in &snapshot.providers {
        if !identifier(&p.id)
            || !providers.insert(&p.id)
            || !text_ok(&p.name, 256)
            || p.name.trim().is_empty()
            || (p.deleted && p.enabled)
            || magpie_core::providers::validate_base_url(&p.base_url, p.allow_insecure_http)
                .is_err()
            || !matches!(
                p.protocol.as_str(),
                "chat" | "responses" | "messages" | "gemini"
            )
            || p.keys.len() > 32
            || p.models.len() > 512
            || p.selected_models.len() > 512
        {
            return false;
        }
        let mut keys = HashSet::new();
        for k in &p.keys {
            if !identifier(&k.id)
                || !keys.insert(&k.id)
                || !text_ok(&k.name, 256)
                || k.name.trim().is_empty()
                || !(if k.deleted {
                    !k.enabled && k.env.is_empty()
                } else {
                    env_ok(&k.env)
                })
                || k.masked != "••••"
            {
                return false;
            }
        }
        let mut models = HashSet::new();
        if p.models.iter().any(|m| !model_ok(m) || !models.insert(m))
            || p.selected_models.iter().any(|m| !models.contains(m))
            || p.selected_models.iter().collect::<HashSet<_>>().len() != p.selected_models.len()
        {
            return false;
        }
        if p.catalog_error.as_ref().is_some_and(|c| !identifier(c)) {
            return false;
        }
    }
    snapshot.operation.as_ref().is_none_or(|op| {
        matches!(op.kind.as_str(), "refresh" | "test")
            && identifier(&op.code)
            && op.http_status.is_none_or(|s| (100..=599).contains(&s))
            && op.elapsed_ms <= 120_000
    })
}
fn decode(success: bool, bytes: &[u8], mutation: bool) -> Result<Snapshot, Failure> {
    if !success {
        let error: ErrorEnvelope = serde_json::from_slice(bytes).map_err(|_| invalid(mutation))?;
        if error.version != "0.2.0"
            || !identifier(&error.error.code)
            || error.error.message.len() > 4096
        {
            return Err(invalid(mutation));
        }
        // Never surface the supplied message or arbitrary code as visible text.
        return Err(Failure {
            code: error.error.code,
            uncertain: mutation,
        });
    }
    let snapshot: Snapshot = serde_json::from_slice(bytes).map_err(|_| invalid(mutation))?;
    if !validate(&snapshot) {
        return Err(invalid(mutation));
    }
    Ok(snapshot)
}
pub(crate) fn store_path() -> Result<PathBuf, Failure> {
    let dir = magpie_core::paths::config_dir().map_err(|_| Failure {
        code: "provider_path_unavailable".into(),
        uncertain: false,
    })?;
    if !dir.is_absolute() {
        return Err(Failure {
            code: "provider_path_unavailable".into(),
            uncertain: false,
        });
    }
    Ok(dir.join("providers.toml"))
}
pub(crate) async fn run(request: Request, cancel: Cancellation) -> Result<Snapshot, Failure> {
    if !matches!(
        request.command,
        "list"
            | "show"
            | "add"
            | "edit"
            | "remove"
            | "key-put"
            | "key-remove"
            | "key-order"
            | "models"
            | "refresh"
            | "test"
            | "recover"
    ) {
        return Err(BridgeError::new(ErrorKind::InvalidReply).into());
    }
    let mutation = request.mutation();
    let mut args: Vec<OsString> = vec!["providers".into(), request.command.into()];
    if let Some(id) = &request.id {
        if !identifier(id) {
            return Err(Failure {
                code: "provider_invalid_id".into(),
                uncertain: false,
            });
        }
        args.push(id.into());
    }
    args.push("--store".into());
    args.push(store_path()?.into_os_string());
    if let Some(token) = &request.token {
        args.push("--expect".into());
        args.push(token.into());
    }
    let input = if let Some(data) = &request.data {
        args.push("--stdin".into());
        Some(serde_json::to_vec(data).map_err(|_| invalid(false))?)
    } else {
        None
    };
    args.push("--json".into());
    let (success, bytes) = cli_bridge::run_provider(args, input, mutation, cancel).await?;
    decode(success, &bytes, mutation)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn empty() -> Value {
        json!({"version":"0.2.0","schema_version":1,"revision":0,"token":format!("0:{}","0".repeat(64)),"providers":[]})
    }
    #[test]
    fn validates_version_and_bounded_snapshot() {
        let good = empty();
        assert!(decode(true, good.to_string().as_bytes(), false).is_ok());
        let mut bad = good;
        bad["version"] = json!("0.3.0");
        assert!(decode(true, bad.to_string().as_bytes(), false).is_err());
    }
    #[test]
    fn malformed_write_success_is_uncertain() {
        let e = decode(true, b"{}", true).unwrap_err();
        assert!(e.uncertain);
    }
    #[test]
    fn operation_failure_is_not_saved_as_success() {
        let mut v = empty();
        v["operation"] = json!({"kind":"test","ok":false,"code":"upstream_unauthorized","http_status":401,"elapsed_ms":20});
        let r = decode(true, v.to_string().as_bytes(), true).unwrap();
        assert!(!r.operation.unwrap().ok);
    }
    #[test]
    fn arbitrary_error_message_is_discarded() {
        let v = json!({"version":"0.2.0","error":{"code":"provider_conflict","message":"private-token-do-not-display"}});
        let e = decode(false, v.to_string().as_bytes(), true).unwrap_err();
        assert_eq!(e.code, "provider_conflict");
        assert!(!format!("{e:?}").contains("private-token"));
    }
}

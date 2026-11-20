//! Uniform Agent command boundary owned by the native Codex plugin.
use crate::{
    agents, codex_config,
    codex_transaction::{self, TransactionError},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    net::SocketAddr,
    path::{Component, Path, PathBuf},
};

#[derive(Clone, Copy)]
enum Operation {
    Selection,
    Preview,
    Connect,
    ModelPreview,
    Model,
    Disconnect,
}
struct Options<'a> {
    operation: Operation,
    name: &'a str,
    config: Option<&'a str>,
    model: Option<&'a str>,
    listen: Option<SocketAddr>,
    revision: Option<&'a str>,
}
impl<'a> Options<'a> {
    fn parse(args: &'a [String]) -> Result<Self, String> {
        let name = args
            .first()
            .ok_or("invalid_operation: Agent plugin operation is required")?
            .as_str();
        let operation = match name {
            "selection" => Operation::Selection,
            "preview" => Operation::Preview,
            "connect" => Operation::Connect,
            "model-preview" => Operation::ModelPreview,
            "model" => Operation::Model,
            "disconnect" => Operation::Disconnect,
            _ => return Err("invalid_operation: unsupported Agent plugin operation".into()),
        };
        let mut options = Self {
            operation,
            name,
            config: None,
            model: None,
            listen: None,
            revision: None,
        };
        let mut args = args.iter().skip(1);
        while let Some(key) = args.next() {
            let value = args
                .next()
                .ok_or("invalid_arguments: an Agent option requires a value")?
                .as_str();
            match key.as_str() {
                "--config" if options.config.is_none() => options.config = Some(value),
                "--model" if options.model.is_none() => options.model = Some(value),
                "--listen" if options.listen.is_none() => {
                    let address: SocketAddr = value
                        .parse()
                        .map_err(|_| "invalid_gateway: expected a literal loopback address")?;
                    if !address.ip().is_loopback() || address.port() == 0 {
                        return Err(
                            "invalid_gateway: expected a loopback address with a nonzero port"
                                .into(),
                        );
                    }
                    options.listen = Some(address);
                }
                "--if-revision" if options.revision.is_none() => options.revision = Some(value),
                _ => {
                    return Err("invalid_arguments: unknown or repeated Agent plugin option".into());
                }
            }
        }
        let permitted = match operation {
            Operation::Selection | Operation::Disconnect => {
                options.model.is_none() && options.listen.is_none() && options.revision.is_none()
            }
            Operation::Preview => {
                options.model.is_some() && options.listen.is_some() && options.revision.is_none()
            }
            Operation::Connect => options.model.is_some() && options.listen.is_some(),
            Operation::ModelPreview => {
                options.model.is_some() && options.listen.is_none() && options.revision.is_none()
            }
            Operation::Model => options.model.is_some() && options.listen.is_none(),
        };
        if !permitted {
            return Err("invalid_arguments: options do not match the Agent operation".into());
        }
        if matches!(operation, Operation::Connect | Operation::Model) && options.revision.is_none()
        {
            return Err("revision_required: apply requires an explicit preview revision".into());
        }
        if options.revision.is_some_and(|s| {
            s.len() != 64
                || !s
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        }) {
            return Err(
                "invalid_revision: expected the opaque revision returned by preview".into(),
            );
        }
        if options.model.is_some_and(|s| !qualified_model(s)) {
            return Err(
                "invalid_model: expected a bounded qualified provider/model identifier".into(),
            );
        }
        if let Some(path) = options.config {
            safe_path(Path::new(path))?;
        }
        Ok(options)
    }
    fn path(&self) -> Result<PathBuf, String> {
        let path = self
            .config
            .map(PathBuf::from)
            .or_else(agents::codex_config_path)
            .ok_or("config_unavailable: Codex configuration path is unavailable")?;
        safe_path(&path)?;
        Ok(path)
    }
}
fn safe_path(path: &Path) -> Result<(), String> {
    if !path.is_absolute()
        || path.file_name().is_none()
        || path.as_os_str().len() > 4096
        || path
            .components()
            .any(|c| !matches!(c, Component::RootDir | Component::Normal(_)))
        || path
            .to_str()
            .is_none_or(|s| s.chars().any(char::is_control))
    {
        return Err("invalid_config_path: expected a bounded absolute configuration path".into());
    }
    Ok(())
}
fn qualified_model(model: &str) -> bool {
    let Some((provider, name)) = model.split_once('/') else {
        return false;
    };
    model.len() <= 512
        && !provider.is_empty()
        && provider.len() <= 128
        && !name.is_empty()
        && provider
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        && name
            .bytes()
            .all(|b| b.is_ascii_graphic() && !b"\\\"'?#".contains(&b))
}
fn public_selection(model: Option<&str>) -> Value {
    match model {
        Some(model) if qualified_model(model) => json!({"model":model,"state":"qualified"}),
        Some(model) if public_value(Some(model)).is_some() => {
            json!({"model":model,"state":"external"})
        }
        Some(_) => json!({"model":null,"state":"unreadable"}),
        None => json!({"model":null,"state":"unset"}),
    }
}
// These are explicitly public selections, never inferred from provider definitions.
fn public_value(value: Option<&str>) -> Option<&str> {
    value.filter(|value| {
        !value.is_empty() && value.len() <= 512 && !value.chars().any(char::is_control)
    })
}
fn public_fields(selection: Option<&codex_config::CodexSelection>) -> Value {
    json!([
        {"id":"model_provider", "label":{
            "en":"Model provider", "zh-CN":"模型供应商", "zh-TW":"模型供應商",
            "ja":"モデルプロバイダー", "de":"Modellanbieter"
        }, "value":public_value(selection.and_then(|s| s.model_provider.as_deref()))},
        {"id":"model_reasoning_effort", "label":{
            "en":"Reasoning effort", "zh-CN":"推理强度", "zh-TW":"推理強度",
            "ja":"推論の強度", "de":"Denkaufwand"
        }, "value":public_value(selection.and_then(|s| s.model_reasoning_effort.as_deref()))}
    ])
}
fn revision(snapshot: &str, operation: &str, model: &str, gateway: Option<SocketAddr>) -> String {
    let mut hash = Sha256::new();
    hash.update(b"magpie-codex-command-preview/1\0");
    for value in [
        snapshot.to_owned(),
        operation.to_owned(),
        model.to_owned(),
        gateway.map(|a| a.to_string()).unwrap_or_default(),
    ] {
        hash.update((value.len() as u64).to_le_bytes());
        hash.update(value.as_bytes());
    }
    format!("{:x}", hash.finalize())
}
fn require_revision(expected: Option<&str>, actual: &str) -> Result<(), String> {
    match expected {
        Some(value) if value == actual => Ok(()),
        Some(_) => Err(
            "config_conflict: configuration or requested operation changed since preview".into(),
        ),
        None => Err("revision_required: apply requires an explicit preview revision".into()),
    }
}
fn transaction_error(error: TransactionError) -> String {
    match error {
        TransactionError::OriginalChanged=>"config_conflict: configuration changed since preview",
        TransactionError::AlreadyManaged=>"already_managed: disconnect before connecting again",
        TransactionError::NotManaged=>"not_managed: no active ownership exists for this configuration",
        TransactionError::StateInvalid|TransactionError::RecoveryRequired|TransactionError::CommitUncertain=>"recovery_required: private recovery state requires inspection; existing data was preserved",
        TransactionError::LockBusy=>"config_busy: another configuration transaction is in progress",
        TransactionError::UnsupportedPlatform=>"unsupported_platform: secure configuration mutation is unavailable",
        _=>"config_transaction_error: Codex configuration could not be accessed safely; existing data was preserved",
    }.into()
}

/// Resolves the same validated target used by execute, so hosts can pin it before apply.
///
/// # Errors
/// Rejects malformed options or an unavailable/unsafe default path with static diagnostics.
pub fn configuration_path(args: &[String]) -> Result<PathBuf, String> {
    Options::parse(args)?.path()
}

/// Executes selection, read-only preview, revision-bound apply, model update or disconnect.
///
/// The host must check owner activation before new work. Explicit recovery may remain
/// available while disabled. Defaults use this plugin's Codex path discovery; only
/// an explicitly supplied path is returned. Apply requires a fresh 64-hex preview
/// revision bound to operation, intent, exact target snapshot and ownership state.
/// No gateway/network liveness is claimed or probed. Model changes retain the endpoint.
///
/// # Errors
/// Errors contain static codes/messages, never configuration values or parser excerpts.
pub fn execute(args: &[String]) -> Result<Value, String> {
    let options = Options::parse(args)?;
    let path = options.path()?;
    let mut output =
        json!({"version":magpie_core::VERSION,"agent_id":"codex","operation":options.name});
    if let Some(path) = options.config {
        output["config_path"] = json!(path);
    }
    match options.operation {
        Operation::Selection => {
            let status = codex_transaction::connection_status(&path).map_err(transaction_error)?;
            let selection = codex_config::read_selection(&path)
                .map_err(|_| "config_read_error: Codex selection could not be read safely")?;
            output["status"] = json!(status.as_str());
            output["selection"] =
                public_selection(selection.as_ref().and_then(|s| s.model.as_deref()));
            output["public_fields"] = public_fields(selection.as_ref());
            output["selection_scope"] = json!("file");
        }
        Operation::Preview | Operation::Connect => {
            let model = options
                .model
                .ok_or("invalid_arguments: model is required")?;
            let address = options
                .listen
                .ok_or("invalid_arguments: listen address is required")?;
            let plan = codex_transaction::prepare_connect(&path, model, address)
                .map_err(transaction_error)?;
            let approved = revision(plan.revision(), "connect", model, Some(address));
            output["revision"] = json!(approved);
            output["changed_fields"] = json!(plan.changed_fields());
            output["selection"] = public_selection(Some(model));
            if matches!(options.operation, Operation::Preview) {
                output["status"] = json!("unmanaged");
                output["selection_scope"] = json!("proposed");
                output["applied"] = json!(false);
            } else {
                require_revision(options.revision, &approved)?;
                let status = codex_transaction::commit_connect(plan).map_err(transaction_error)?;
                output["status"] = json!(status.as_str());
                output["selection_scope"] = json!("applied");
                output["applied"] = json!(true);
            }
        }
        Operation::ModelPreview | Operation::Model => {
            let model = options
                .model
                .ok_or("invalid_arguments: model is required")?;
            let plan =
                codex_transaction::prepare_model_update(&path, model).map_err(transaction_error)?;
            let approved = revision(plan.revision(), "model", model, Some(plan.gateway_addr()));
            output["revision"] = json!(approved);
            output["changed_fields"] = json!(plan.changed_fields());
            output["selection"] = public_selection(Some(model));
            if matches!(options.operation, Operation::ModelPreview) {
                output["status"] = json!("connected");
                output["selection_scope"] = json!("proposed");
                output["applied"] = json!(false);
            } else {
                require_revision(options.revision, &approved)?;
                let result =
                    codex_transaction::commit_model_update(plan).map_err(transaction_error)?;
                output["status"] = json!(result.status.as_str());
                output["selection_scope"] = json!("applied");
                output["applied"] = json!(true);
            }
        }
        Operation::Disconnect => {
            let result = match codex_transaction::disconnect(&path) {
                Ok(result) => result,
                Err(TransactionError::NotManaged)
                    if matches!(
                        codex_transaction::connection_status(&path),
                        Ok(codex_transaction::ConnectionStatus::Unmanaged)
                    ) =>
                {
                    codex_transaction::DisconnectOutcome {
                        restored_fields: 0,
                        preserved_fields: 0,
                        config_removed: false,
                    }
                }
                Err(error) => return Err(transaction_error(error)),
            };
            output["status"] = json!("unmanaged");
            output["selection"] = json!({"model":null,"state":"not_reported"});
            output["selection_scope"] = json!("not_reported");
            output["restored_fields"] = json!(result.restored_fields);
            output["preserved_fields"] = json!(result.preserved_fields);
            output["config_removed"] = json!(result.config_removed);
            output["applied"] = json!(true);
        }
    }
    Ok(output)
}

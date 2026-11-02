//! CLI boundary for declarative Agent plugins; never discovers running instances.
//!
//! The caller checks plugin activation. Retained definitions may still execute an
//! explicit disconnect after deactivation. This module never edits plugin state.
use std::collections::BTreeSet;
use std::env;
use std::net::SocketAddr;
use std::path::{Component, Path, PathBuf};

use magpie_core::agent_connection::{self, AgentDefinition, ConnectionError, ConnectionStatus};
use serde_json::{Value, json};

/// Execute a plugin operation using a caller-selected definition.
///
/// `args[0]` is selection, preview, connect, model-preview, model or disconnect.
/// Connect previews require --model and --listen; model previews retain the
/// existing endpoint. Connect/model applies require --if-revision and prepare a
/// fresh plan before comparing the token. Returned apply revisions identify the
/// consumed preview, so every later change needs another preview.
///
/// Only --config supplied explicitly by the caller is included in output. Default
/// path hints expand HOME (including the manifest's lowercase alias) and
/// XDG_CONFIG_HOME only. Multiple existing paths require an explicit target.
/// All failures contain a static code/message, never parser snippets or raw values.
pub fn execute(definition: &AgentDefinition, args: &[String]) -> Result<Value, String> {
    execute_with_paths(definition, args, &EnvironmentPaths::system())
}

/// Resolve the target of a complete operation without writing any files.
///
/// Hosts may privately pin this target and its original definition before a
/// connect apply. The result is not an installation or running-instance claim.
/// Preserve an existing pin rather than replacing it on plugin upgrade or failure.
pub fn configuration_path(
    definition: &AgentDefinition,
    args: &[String],
) -> Result<PathBuf, String> {
    definition.validate().map_err(connection_error)?;
    let operation = Operation::parse(
        args.first()
            .ok_or_else(|| error("invalid_operation", "Agent plugin operation is required"))?,
    )?;
    let options = Options::parse(&args[1..])?;
    options.validate(operation)?;
    options.listen.map(gateway).transpose()?;
    selected_path(definition, options.config, &EnvironmentPaths::system())
}

/// Refuse plugin deactivation while any selected file retains Magpie ownership.
///
/// Only --config ABS is accepted. Without it all existing whitelist-resolvable
/// default candidates are checked, rather than selecting one. All read/parse/lock
/// uncertainty is an error. A host must also check every retained explicit target:
/// this cannot prove the absence of connections at unknown custom paths. Keep the
/// original definition and recovery images until an explicit disconnect succeeds.
pub fn ensure_not_in_use(definition: &AgentDefinition, args: &[String]) -> Result<(), String> {
    ensure_not_in_use_with_paths(definition, args, &EnvironmentPaths::system())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Operation {
    Selection,
    Preview,
    Connect,
    ModelPreview,
    Model,
    Disconnect,
}
impl Operation {
    fn parse(s: &str) -> Result<Self, String> {
        match s {
            "selection" => Ok(Self::Selection),
            "preview" => Ok(Self::Preview),
            "connect" => Ok(Self::Connect),
            "model-preview" => Ok(Self::ModelPreview),
            "model" => Ok(Self::Model),
            "disconnect" => Ok(Self::Disconnect),
            _ => Err(error(
                "invalid_operation",
                "unsupported Agent plugin operation",
            )),
        }
    }
}
#[derive(Default)]
struct Options<'a> {
    config: Option<&'a str>,
    model: Option<&'a str>,
    listen: Option<&'a str>,
    revision: Option<&'a str>,
}
impl<'a> Options<'a> {
    fn parse(args: &'a [String]) -> Result<Self, String> {
        if args.len() > 16 || args.iter().any(|s| s.len() > 4096) {
            return Err(error(
                "invalid_arguments",
                "Agent plugin arguments exceed limits",
            ));
        }
        let mut options = Self::default();
        let mut args = args.iter();
        while let Some(flag) = args.next() {
            let slot = match flag.as_str() {
                "--config" => &mut options.config,
                "--model" => &mut options.model,
                "--listen" => &mut options.listen,
                "--if-revision" => &mut options.revision,
                _ => return Err(error("invalid_arguments", "unknown Agent plugin option")),
            };
            let value = args.next().ok_or_else(|| {
                error("invalid_arguments", "Agent plugin option requires a value")
            })?;
            if slot.is_some() || value.is_empty() || value.starts_with("--") {
                return Err(error(
                    "invalid_arguments",
                    "duplicate or missing Agent plugin option",
                ));
            }
            *slot = Some(value.as_str());
        }
        Ok(options)
    }
    fn validate(&self, operation: Operation) -> Result<(), String> {
        let permitted = match operation {
            Operation::Selection | Operation::Disconnect => {
                self.model.is_none() && self.listen.is_none() && self.revision.is_none()
            }
            Operation::Preview => {
                self.listen.is_some() && self.model.is_some() && self.revision.is_none()
            }
            Operation::Connect => self.listen.is_some() && self.model.is_some(),
            Operation::ModelPreview => {
                self.listen.is_none() && self.model.is_some() && self.revision.is_none()
            }
            Operation::Model => self.listen.is_none() && self.model.is_some(),
        };
        if !permitted {
            return Err(error(
                "invalid_arguments",
                "options do not match the requested Agent operation",
            ));
        }
        if matches!(operation, Operation::Connect | Operation::Model) && self.revision.is_none() {
            return Err(error(
                "revision_required",
                "apply requires an explicit preview revision",
            ));
        }
        if self.revision.is_some_and(|s| {
            let parts: Vec<_> = s.split('.').collect();
            s.len() > 2048
                || parts.len() != 3
                || parts[0] != "v1"
                || parts[1].len() != 64
                || parts[2].is_empty()
                || parts[2].len() % 2 != 0
                || parts[1..].iter().any(|s| {
                    !s.bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                })
        }) {
            return Err(error(
                "invalid_revision",
                "preview revision must be an opaque token returned by this CLI",
            ));
        }
        if self.model.is_some_and(|s| !qualified_model(s)) {
            return Err(error(
                "invalid_model",
                "model must be a bounded qualified provider/model identifier",
            ));
        }
        Ok(())
    }
}
fn qualified_model(model: &str) -> bool {
    if model.is_empty()
        || model.len() > 512
        || !model.is_ascii()
        || model
            .bytes()
            .any(|b| b.is_ascii_whitespace() || b.is_ascii_control())
    {
        return false;
    }
    let Some((provider, name)) = model.split_once('/') else {
        return false;
    };
    !provider.is_empty()
        && provider.len() <= 128
        && !name.is_empty()
        && provider
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        && !name.bytes().any(|b| b"\\\"'?#".contains(&b))
}
fn gateway(value: &str) -> Result<SocketAddr, String> {
    let address: SocketAddr = value.parse().map_err(|_| {
        error(
            "invalid_gateway",
            "gateway must be a literal loopback SocketAddr",
        )
    })?;
    if address.port() == 0
        || !address.ip().is_loopback()
        || matches!(address, SocketAddr::V6(v6) if v6.scope_id()!=0 || v6.flowinfo()!=0)
    {
        return Err(error(
            "invalid_gateway",
            "gateway must be loopback with a nonzero port and no IPv6 scope",
        ));
    }
    Ok(address)
}
fn public_selection(model: Option<&str>) -> Value {
    match model {
        Some(model) if qualified_model(model) => json!({"model":model,"state":"qualified"}),
        Some(model)
            if !model.is_empty() && model.len() <= 512 && !model.chars().any(char::is_control) =>
        {
            json!({"model":model,"state":"external"})
        }
        Some(_) => json!({"model":null,"state":"unreadable"}),
        None => json!({"model":null,"state":"unset"}),
    }
}
fn response(definition: &AgentDefinition, operation: &str, options: &Options<'_>) -> Value {
    let mut value =
        json!({"version":magpie_core::VERSION,"agent_id":definition.id,"operation":operation});
    if let Some(path) = options.config {
        value["config_path"] = json!(path);
    }
    value
}
fn execute_with_paths(
    definition: &AgentDefinition,
    args: &[String],
    environment: &EnvironmentPaths,
) -> Result<Value, String> {
    definition.validate().map_err(connection_error)?;
    let operation_name = args
        .first()
        .ok_or_else(|| error("invalid_operation", "Agent plugin operation is required"))?;
    let operation = Operation::parse(operation_name)?;
    let options = Options::parse(&args[1..])?;
    options.validate(operation)?;
    // Validate public inputs before any filesystem read.
    let address = options.listen.map(gateway).transpose()?;
    let path = selected_path(definition, options.config, environment)?;
    let mut result = response(definition, operation_name, &options);
    match operation {
        Operation::Selection => {
            let selection =
                agent_connection::read_selection(definition, &path).map_err(connection_error)?;
            result["status"] = json!(selection.status.as_str());
            result["selection"] = public_selection(selection.model.as_deref());
            result["selection_scope"] = json!("file");
        }
        Operation::Preview | Operation::Connect => {
            let model = options
                .model
                .ok_or_else(|| error("invalid_arguments", "model is required"))?;
            let address =
                address.ok_or_else(|| error("invalid_arguments", "listen address is required"))?;
            let plan = agent_connection::prepare_connect(definition, &path, model, address)
                .map_err(connection_error)?;
            let approved_revision =
                bound_revision(plan.revision(), "connect", model, Some(address));
            result["revision"] = json!(approved_revision);
            result["changed_fields"] = json!(plan.changed_fields());
            result["selection"] = public_selection(Some(model));
            if operation == Operation::Preview {
                result["status"] = json!("unmanaged");
                result["selection_scope"] = json!("proposed");
                result["applied"] = json!(false);
            } else {
                require_revision(options.revision, &approved_revision)?;
                let status = agent_connection::commit_connect(plan).map_err(connection_error)?;
                result["status"] = json!(status.as_str());
                result["selection_scope"] = json!("applied");
                result["applied"] = json!(true);
            }
        }
        Operation::ModelPreview | Operation::Model => {
            let model = options
                .model
                .ok_or_else(|| error("invalid_arguments", "model is required"))?;
            let plan = agent_connection::prepare_model_update(definition, &path, model)
                .map_err(connection_error)?;
            let approved_revision = bound_revision(plan.revision(), "model", model, None);
            result["revision"] = json!(approved_revision);
            result["changed_fields"] = json!(plan.changed_fields());
            result["selection"] = public_selection(Some(model));
            if operation == Operation::ModelPreview {
                result["status"] = json!("connected");
                result["selection_scope"] = json!("proposed");
                result["applied"] = json!(false);
            } else {
                require_revision(options.revision, &approved_revision)?;
                let status =
                    agent_connection::commit_model_update(plan).map_err(connection_error)?;
                result["status"] = json!(status.as_str());
                result["selection_scope"] = json!("applied");
                result["applied"] = json!(true);
            }
        }
        Operation::Disconnect => {
            let outcome =
                agent_connection::disconnect(definition, &path).map_err(connection_error)?;
            result["status"] = json!("unmanaged");
            result["selection"] = json!({"model":null,"state":"not_reported"});
            result["selection_scope"] = json!("not_reported");
            result["restored_fields"] = json!(outcome.restored_fields);
            result["preserved_fields"] = json!(outcome.preserved_fields);
            result["config_removed"] = json!(outcome.config_removed);
            result["applied"] = json!(true);
        }
    }
    Ok(result)
}
fn bound_revision(
    snapshot: &str,
    operation: &str,
    model: &str,
    gateway: Option<SocketAddr>,
) -> String {
    // These are already validated public inputs. Keep the core snapshot opaque
    // and bind preview intent without adding cryptography or dependency changes.
    let parameters = json!([operation, model, gateway.map(|s| s.to_string())]).to_string();
    let mut encoded = String::with_capacity(parameters.len() * 2);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for b in parameters.bytes() {
        encoded.push(char::from(HEX[usize::from(b >> 4)]));
        encoded.push(char::from(HEX[usize::from(b & 15)]));
    }
    format!("v1.{snapshot}.{encoded}")
}
fn require_revision(expected: Option<&str>, actual: &str) -> Result<(), String> {
    match expected {
        None => Err(error(
            "revision_required",
            "apply requires an explicit preview revision",
        )),
        Some(expected) if expected != actual => Err(error(
            "config_conflict",
            "configuration changed since the approved preview",
        )),
        Some(_) => Ok(()),
    }
}
#[derive(Default)]
struct EnvironmentPaths {
    home: Option<PathBuf>,
    xdg_config_home: Option<PathBuf>,
}
impl EnvironmentPaths {
    fn system() -> Self {
        Self {
            home: env::var_os("HOME")
                .filter(|s| !s.is_empty())
                .map(PathBuf::from),
            xdg_config_home: env::var_os("XDG_CONFIG_HOME")
                .filter(|s| !s.is_empty())
                .map(PathBuf::from),
        }
    }
}
fn safe_absolute(path: &Path) -> Result<(), String> {
    if !path.is_absolute()
        || path.components().any(|c| matches!(c, Component::ParentDir))
        || path
            .as_os_str()
            .to_str()
            .is_none_or(|s| s.chars().any(char::is_control))
    {
        return Err(error(
            "config_path_required",
            "configuration target must be an absolute path without traversal or control characters",
        ));
    }
    Ok(())
}
fn expand_hint(hint: &str, environment: &EnvironmentPaths) -> Result<Option<PathBuf>, String> {
    if hint.starts_with('~') {
        return Ok(None);
    }
    let xdg = environment
        .xdg_config_home
        .clone()
        .or_else(|| environment.home.as_ref().map(|p| p.join(".config")));
    let mut remaining = hint;
    let mut expanded = String::new();
    while let Some(index) = remaining.find('$') {
        expanded.push_str(&remaining[..index]);
        remaining = &remaining[index + 1..];
        let variable = if let Some(braced) = remaining.strip_prefix('{') {
            let Some(end) = braced.find('}') else {
                return Ok(None);
            };
            let name = &braced[..end];
            remaining = &braced[end + 1..];
            name
        } else {
            let end = remaining
                .bytes()
                .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_')
                .count();
            let name = &remaining[..end];
            remaining = &remaining[end..];
            name
        };
        let path = match variable {
            "HOME" | "home" => environment.home.as_ref(),
            "XDG_CONFIG_HOME" => xdg.as_ref(),
            _ => return Ok(None),
        };
        let Some(path) = path else { return Ok(None) };
        safe_absolute(path)?;
        expanded.push_str(path.to_str().ok_or_else(|| {
            error(
                "config_path_required",
                "environment configuration path must be representable as UTF-8",
            )
        })?);
    }
    expanded.push_str(remaining);
    let path = PathBuf::from(expanded);
    safe_absolute(&path)?;
    Ok(Some(path))
}
fn existing_candidates(
    definition: &AgentDefinition,
    environment: &EnvironmentPaths,
) -> Result<Vec<PathBuf>, String> {
    let mut paths = BTreeSet::new();
    for hint in &definition.config_paths {
        let Some(path) = expand_hint(hint, environment)? else {
            continue;
        };
        match std::fs::symlink_metadata(&path) {
            Ok(_) => {
                paths.insert(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => {
                return Err(error(
                    "config_read_error",
                    "configuration candidate metadata could not be read",
                ));
            }
        }
    }
    Ok(paths.into_iter().collect())
}
fn selected_path(
    definition: &AgentDefinition,
    explicit: Option<&str>,
    environment: &EnvironmentPaths,
) -> Result<PathBuf, String> {
    if let Some(path) = explicit {
        let path = PathBuf::from(path);
        safe_absolute(&path)?;
        return Ok(path);
    }
    let mut candidates = existing_candidates(definition, environment)?;
    match candidates.len() {
        0 => Err(error(
            "config_missing",
            "no existing whitelist-resolvable configuration; provide an explicit target",
        )),
        1 => Ok(candidates.remove(0)),
        _ => Err(error(
            "config_ambiguous",
            "multiple configuration files exist; provide an explicit target without guessing a running instance",
        )),
    }
}
fn ensure_not_in_use_with_paths(
    definition: &AgentDefinition,
    args: &[String],
    environment: &EnvironmentPaths,
) -> Result<(), String> {
    definition.validate().map_err(connection_error)?;
    let options = Options::parse(args)?;
    if options.model.is_some() || options.listen.is_some() || options.revision.is_some() {
        return Err(error(
            "invalid_arguments",
            "deactivation guard accepts only the --config option",
        ));
    }
    let paths = if let Some(path) = options.config {
        let path = PathBuf::from(path);
        safe_absolute(&path)?;
        vec![path]
    } else {
        existing_candidates(definition, environment)?
    };
    for path in paths {
        let selection =
            agent_connection::read_selection(definition, &path).map_err(connection_error)?;
        match selection.status {
            ConnectionStatus::Unmanaged => {}
            ConnectionStatus::Connected
            | ConnectionStatus::Drifted
            | ConnectionStatus::RecoveryRequired => {
                return Err(error(
                    "plugin_in_use",
                    "disconnect the Agent configuration before disabling its owner",
                ));
            }
        }
    }
    Ok(())
}
fn error(code: &'static str, message: &'static str) -> String {
    format!("{code}: {message}")
}
fn connection_error(cause: ConnectionError) -> String {
    let (code, message) = match cause {
        ConnectionError::UnsupportedPlatform => (
            "unsupported_platform",
            "native configuration transactions are unavailable on this platform",
        ),
        ConnectionError::UnsafePath => ("config_unsafe_path", "configuration path is unsafe"),
        ConnectionError::MissingParent | ConnectionError::MissingConfig => {
            ("config_missing", "configuration file or parent is missing")
        }
        ConnectionError::NotAFile => (
            "config_not_a_file",
            "configuration or recovery material is not a regular single-link file",
        ),
        ConnectionError::Symlink => (
            "config_symlink",
            "symlink configuration or recovery paths are refused",
        ),
        ConnectionError::TooLarge => (
            "config_too_large",
            "configuration or recovery data exceeds the size limit",
        ),
        ConnectionError::InvalidUtf8 => ("config_invalid_utf8", "configuration is not UTF-8"),
        ConnectionError::InvalidJson | ConnectionError::UnsupportedShape => (
            "config_parse_error",
            "configuration syntax or managed field shape is unsupported",
        ),
        ConnectionError::ReadOnly => (
            "config_read_only",
            "configuration or recovery directory is read-only",
        ),
        ConnectionError::Read => (
            "config_read_error",
            "configuration or recovery state could not be read",
        ),
        ConnectionError::Write => (
            "config_write_error",
            "configuration transaction could not be persisted",
        ),
        ConnectionError::LockBusy => (
            "config_conflict",
            "another configuration transaction holds the OS lock",
        ),
        ConnectionError::InvalidDefinition => (
            "invalid_agent_definition",
            "declarative Agent definition is invalid",
        ),
        ConnectionError::UnknownProvider => (
            "config_conflict",
            "an existing unowned configuration block cannot be adopted",
        ),
        ConnectionError::InvalidModel => (
            "invalid_model",
            "model must be a bounded qualified provider/model identifier",
        ),
        ConnectionError::InvalidGateway => (
            "invalid_gateway",
            "gateway must be a literal loopback SocketAddr",
        ),
        ConnectionError::OriginalChanged => (
            "config_conflict",
            "configuration changed since preview or owned fields have drifted",
        ),
        ConnectionError::AlreadyManaged => (
            "config_conflict",
            "configuration is already managed; use a model preview",
        ),
        ConnectionError::NotManaged => (
            "config_not_managed",
            "no active ownership record exists for this configuration",
        ),
        ConnectionError::StateInvalid => (
            "config_state_invalid",
            "private recovery state is invalid or requires the original definition",
        ),
        ConnectionError::RecoveryRequired => (
            "config_recovery_required",
            "an interrupted transaction requires explicit recovery",
        ),
        ConnectionError::CommitUncertain => (
            "config_commit_uncertain",
            "configuration may have changed; retain recovery materials and inspect status",
        ),
        ConnectionError::StorageLimit => (
            "config_storage_limit",
            "private recovery storage limit reached",
        ),
    };
    error(code, message)
}

#[cfg(all(
    test,
    any(target_os = "linux", target_os = "android", target_os = "macos")
))]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    const ORIGINAL:&[u8]=b"{\n  \"model\" : \"original/model\",\n  \"env\": {\"UNKNOWN\": \"fixture-secret-never-output\"},\n  \"unknown\": true\n}\n";
    struct Fixture {
        root: PathBuf,
        path: PathBuf,
    }
    impl Fixture {
        fn new(bytes: &[u8]) -> Self {
            let root = env::temp_dir().join(format!(
                "magpie-cli-agent-fixture-{}-{}",
                std::process::id(),
                SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            fs::DirBuilder::new()
                .mode(0o700)
                .create(&root)
                .expect("private fixture");
            let path = root.join("config.json");
            fs::write(&path, bytes).expect("synthetic file");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("permissions");
            Self { root, path }
        }
        fn bytes(&self) -> Vec<u8> {
            fs::read(&self.path).expect("fixture config")
        }
        fn config(&self) -> String {
            self.path.to_str().expect("synthetic UTF-8 path").to_owned()
        }
        fn environment(&self) -> EnvironmentPaths {
            EnvironmentPaths {
                home: Some(self.root.clone()),
                xdg_config_home: None,
            }
        }
        fn call(
            &self,
            definition: &AgentDefinition,
            op: &str,
            extra: &[&str],
        ) -> Result<Value, String> {
            let mut args = vec![op.to_owned(), "--config".to_owned(), self.config()];
            args.extend(extra.iter().map(|s| (*s).to_owned()));
            execute(definition, &args)
        }
        fn connect(&self, definition: &AgentDefinition) -> Value {
            let preview = self
                .call(
                    definition,
                    "preview",
                    &["--model", "fixture/model-a", "--listen", "127.0.0.1:43197"],
                )
                .expect("preview");
            self.call(
                definition,
                "connect",
                &[
                    "--model",
                    "fixture/model-a",
                    "--listen",
                    "127.0.0.1:43197",
                    "--if-revision",
                    preview["revision"].as_str().expect("token"),
                ],
            )
            .expect("connect")
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.root).expect("clean synthetic fixture");
        }
    }
    fn definitions() -> Vec<AgentDefinition> {
        [
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../plugins/agents/claude/definition.json"
            )),
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../plugins/agents/opencode/definition.json"
            )),
        ]
        .iter()
        .map(|s| AgentDefinition::from_manifest_json(s).expect("plugin definition"))
        .collect()
    }
    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| (*s).to_owned()).collect()
    }
    #[test]
    fn preview_is_read_only_and_each_plugin_completes_the_synthetic_lifecycle() {
        for definition in definitions() {
            let fixture = Fixture::new(ORIGINAL);
            let preview = fixture
                .call(
                    &definition,
                    "preview",
                    &["--model", "fixture/model-a", "--listen", "127.0.0.1:43197"],
                )
                .expect("preview");
            assert_eq!(preview["version"], "0.2.0");
            assert_eq!(preview["agent_id"], definition.id);
            assert_eq!(preview["operation"], "preview");
            assert_eq!(preview["status"], "unmanaged");
            assert_eq!(preview["applied"], false);
            assert!(preview["changed_fields"].as_array().expect("fields").len() > 1);
            assert_eq!(fixture.bytes(), ORIGINAL);
            assert_eq!(fs::read_dir(&fixture.root).expect("directory").count(), 1);
            assert!(!preview.to_string().contains("fixture-secret-never-output"));
            let applied = fixture.connect(&definition);
            assert_eq!(applied["applied"], true);
            assert_eq!(applied["status"], "connected");
            let selection = fixture
                .call(&definition, "selection", &[])
                .expect("selection");
            assert_eq!(selection["selection"]["model"], "fixture/model-a");
            let before = fixture.bytes();
            let model_preview = fixture
                .call(
                    &definition,
                    "model-preview",
                    &["--model", "fixture/model-b"],
                )
                .expect("model preview");
            assert_eq!(fixture.bytes(), before);
            fixture
                .call(
                    &definition,
                    "model",
                    &[
                        "--model",
                        "fixture/model-b",
                        "--if-revision",
                        model_preview["revision"].as_str().expect("token"),
                    ],
                )
                .expect("model apply");
            assert_eq!(
                fixture
                    .call(&definition, "selection", &[])
                    .expect("selection")["selection"]["model"],
                "fixture/model-b"
            );
            let output = fixture
                .call(&definition, "disconnect", &[])
                .expect("disconnect");
            assert_eq!(output["status"], "unmanaged");
            assert_eq!(output["preserved_fields"], 0);
            assert!(output["restored_fields"].as_u64().expect("count") > 0);
            assert!(output["selection"]["model"].is_null());
            assert!(!output.to_string().contains("original/model"));
            assert!(!output.to_string().contains("fixture-secret-never-output"));
            assert_eq!(fixture.bytes(), ORIGINAL);
        }
    }
    #[test]
    fn applying_requires_a_fresh_revision_bound_to_model_gateway_and_operation() {
        for definition in definitions() {
            let fixture = Fixture::new(ORIGINAL);
            let error = fixture
                .call(
                    &definition,
                    "connect",
                    &["--model", "fixture/model-a", "--listen", "127.0.0.1:43197"],
                )
                .expect_err("revision required");
            assert!(error.starts_with("revision_required:"));
            let preview = fixture
                .call(
                    &definition,
                    "preview",
                    &["--model", "fixture/model-a", "--listen", "127.0.0.1:43197"],
                )
                .expect("preview");
            let revision = preview["revision"].as_str().expect("token");
            for (model, listen) in [
                ("fixture/model-b", "127.0.0.1:43197"),
                ("fixture/model-a", "127.0.0.1:43198"),
            ] {
                let error = fixture
                    .call(
                        &definition,
                        "connect",
                        &[
                            "--model",
                            model,
                            "--listen",
                            listen,
                            "--if-revision",
                            revision,
                        ],
                    )
                    .expect_err("intent mismatch");
                assert!(error.starts_with("config_conflict:"));
                assert_eq!(fixture.bytes(), ORIGINAL);
            }
            let changed = [ORIGINAL, b" "].concat();
            fs::write(&fixture.path, &changed).expect("external edit");
            let error = fixture
                .call(
                    &definition,
                    "connect",
                    &[
                        "--model",
                        "fixture/model-a",
                        "--listen",
                        "127.0.0.1:43197",
                        "--if-revision",
                        revision,
                    ],
                )
                .expect_err("stale preview");
            assert!(error.starts_with("config_conflict:"));
            assert_eq!(fixture.bytes(), changed);
            fixture.connect(&definition);
            let preview = fixture
                .call(
                    &definition,
                    "model-preview",
                    &["--model", "fixture/model-b"],
                )
                .expect("preview");
            let stale = preview["revision"].as_str().expect("token");
            let other = fixture
                .call(
                    &definition,
                    "model-preview",
                    &["--model", "fixture/model-c"],
                )
                .expect("other");
            fixture
                .call(
                    &definition,
                    "model",
                    &[
                        "--model",
                        "fixture/model-c",
                        "--if-revision",
                        other["revision"].as_str().expect("token"),
                    ],
                )
                .expect("other apply");
            assert!(
                fixture
                    .call(
                        &definition,
                        "model",
                        &["--model", "fixture/model-b", "--if-revision", stale]
                    )
                    .expect_err("old ownership revision")
                    .starts_with("config_conflict:")
            );
            fixture
                .call(&definition, "disconnect", &[])
                .expect("disconnect");
            assert_eq!(fixture.bytes(), changed);
        }
    }
    #[test]
    fn external_selection_is_public_while_environment_and_errors_are_redacted() {
        for definition in definitions() {
            for model in [
                "fixture-secret-never-output",
                "sk-fixture-key?private",
                "Bearer synthetic-token",
            ] {
                let file = json!({"model":model,"env":{"PRIVATE_ENV_KEY_CANARY":"PRIVATE_ENV_VALUE_CANARY"}})
                    .to_string();
                let fixture = Fixture::new(file.as_bytes());
                let output = fixture
                    .call(&definition, "selection", &[])
                    .expect("selection");
                assert_eq!(
                    output["selection"],
                    json!({"model":model,"state":"external"})
                );
                assert!(!output.to_string().contains("PRIVATE_ENV_KEY_CANARY"));
                assert!(!output.to_string().contains("PRIVATE_ENV_VALUE_CANARY"));
            }
            let fixture = Fixture::new(b"{ invalid fixture-secret-never-output");
            let error = fixture
                .call(&definition, "selection", &[])
                .expect_err("parse error");
            assert!(error.starts_with("config_parse_error:"));
            assert!(!error.contains("fixture-secret"));
            let error = execute(&definition, &strings(&["fixture-secret-never-output"]))
                .expect_err("unknown operation");
            assert!(!error.contains("fixture-secret"));
        }
    }
    #[test]
    fn default_paths_expand_only_whitelist_require_explicit_ambiguity_and_hide_inferred_paths() {
        let fixture = Fixture::new(ORIGINAL);
        let environment = fixture.environment();
        let mut definition = definitions().remove(0);
        definition.config_paths = vec![
            "${UNAUTHORIZED_AGENT_SECRET}/not-read.json".into(),
            "${HOME}/config.json".into(),
            "${home}/config.json".into(),
        ];
        let output = execute_with_paths(&definition, &strings(&["selection"]), &environment)
            .expect("one unique path");
        assert_eq!(output["selection"]["model"], "original/model");
        assert!(output.get("config_path").is_none());
        assert!(
            !output
                .to_string()
                .contains(fixture.root.to_str().expect("fixture path"))
        );
        let second = fixture.root.join("second.json");
        fs::write(&second, b"{}").expect("second config");
        definition.config_paths.push("${HOME}/second.json".into());
        assert!(
            execute_with_paths(&definition, &strings(&["selection"]), &environment)
                .expect_err("ambiguous")
                .starts_with("config_ambiguous:")
        );
        let output = fixture
            .call(&definition, "selection", &[])
            .expect("explicit");
        assert_eq!(output["config_path"], fixture.config());
        definition.config_paths = vec!["${XDG_CONFIG_HOME}/config.json".into()];
        let xdg = EnvironmentPaths {
            home: None,
            xdg_config_home: Some(fixture.root.clone()),
        };
        assert!(execute_with_paths(&definition, &strings(&["selection"]), &xdg).is_ok());
        assert!(
            execute_with_paths(
                &definition,
                &strings(&["selection"]),
                &EnvironmentPaths::default()
            )
            .expect_err("missing env")
            .starts_with("config_missing:")
        );
        assert!(
            expand_hint("$HOME_SECRET/private.json", &environment)
                .expect("unknown variable")
                .is_none()
        );
        assert!(
            expand_hint("${HOME_SECRET}/private.json", &environment)
                .expect("unknown braced variable")
                .is_none()
        );
        let literal_home = EnvironmentPaths {
            home: Some(fixture.root.join("home$literal")),
            xdg_config_home: None,
        };
        assert_eq!(
            expand_hint("${HOME}/config.json", &literal_home)
                .expect("literal path")
                .expect("home"),
            fixture.root.join("home$literal/config.json")
        );
        let fallback = expand_hint("${XDG_CONFIG_HOME}/app.json", &environment)
            .expect("expand")
            .expect("fallback");
        assert_eq!(fallback, fixture.root.join(".config/app.json"));
    }
    #[test]
    fn disabled_owner_guard_blocks_connected_drifted_and_unreadable_recovery_and_allows_restoration()
     {
        for mut definition in definitions() {
            let fixture = Fixture::new(ORIGINAL);
            definition.config_paths = vec!["${HOME}/config.json".into()];
            let guard = strings(&["--config", &fixture.config()]);
            ensure_not_in_use(&definition, &guard).expect("unmanaged");
            fixture.connect(&definition);
            assert!(
                ensure_not_in_use(&definition, &guard)
                    .expect_err("connected")
                    .starts_with("plugin_in_use:")
            );
            assert!(
                ensure_not_in_use_with_paths(&definition, &[], &fixture.environment())
                    .expect_err("default still connected")
                    .starts_with("plugin_in_use:")
            );
            let text = String::from_utf8(fixture.bytes())
                .expect("utf8")
                .replace("fixture/model-a", "external/model");
            fs::write(&fixture.path, text).expect("drift");
            assert!(
                ensure_not_in_use(&definition, &guard)
                    .expect_err("drift")
                    .starts_with("plugin_in_use:")
            );
            // Dispatch/activation belongs to the host: this API deliberately accepts
            // the retained definition to disconnect even if its owner is disabled.
            fixture
                .call(&definition, "disconnect", &[])
                .expect("restore retained definition");
            ensure_not_in_use(&definition, &guard).expect("unmanaged after restore");
            fs::write(&fixture.path, b"{ malformed fixture-secret-never-output")
                .expect("corrupt config");
            let error =
                ensure_not_in_use(&definition, &guard).expect_err("uncertain state blocks disable");
            assert!(error.starts_with("config_parse_error:"));
            assert!(!error.contains("fixture-secret"));
        }
    }
    #[test]
    fn guard_checks_every_default_candidate_without_guessing_a_running_instance() {
        let fixture = Fixture::new(ORIGINAL);
        let mut definition = definitions().remove(0);
        definition.config_paths = vec!["${HOME}/config.json".into(), "${HOME}/second.json".into()];
        let second = fixture.root.join("second.json");
        fs::write(&second, b"{}").expect("second fixture");
        ensure_not_in_use_with_paths(&definition, &[], &fixture.environment())
            .expect("both unmanaged");
        fixture.connect(&definition);
        assert!(
            ensure_not_in_use_with_paths(&definition, &[], &fixture.environment())
                .expect_err("one active blocks disable")
                .starts_with("plugin_in_use:")
        );
        fixture
            .call(&definition, "disconnect", &[])
            .expect("restore");
    }
    #[test]
    fn syntax_qualified_model_gateway_and_absolute_target_validation_precede_writes() {
        let definition = definitions().remove(0);
        let fixture = Fixture::new(ORIGINAL);
        for extra in [
            vec!["--model", "unqualified", "--listen", "127.0.0.1:43197"],
            vec![
                "--model",
                "fixture/key?secret",
                "--listen",
                "127.0.0.1:43197",
            ],
            vec!["--model", "fixture/model", "--listen", "192.0.2.1:43197"],
            vec!["--model", "fixture/model", "--listen", "localhost:43197"],
            vec!["--model", "fixture/model", "--listen", "127.0.0.1:0"],
            vec![
                "--model",
                "fixture/model",
                "--listen",
                "127.0.0.1:43197",
                "--model",
                "fixture/other",
            ],
        ] {
            let error = fixture
                .call(&definition, "preview", &extra)
                .expect_err("invalid argument");
            assert!(!error.contains("key?secret"));
        }
        assert!(
            execute(
                &definition,
                &strings(&["selection", "--config", "relative.json"])
            )
            .expect_err("absolute path required")
            .starts_with("config_path_required:")
        );
        assert!(
            execute(
                &definition,
                &strings(&["selection", "--config", "/tmp/../private.json"])
            )
            .is_err()
        );
        assert!(
            fixture
                .call(
                    &definition,
                    "model-preview",
                    &["--model", "fixture/model", "--listen", "127.0.0.1:1"]
                )
                .is_err()
        );
        assert!(
            fixture
                .call(
                    &definition,
                    "selection",
                    &["--if-revision", "private-token-sentinel"]
                )
                .is_err()
        );
        assert_eq!(fixture.bytes(), ORIGINAL);
        assert_eq!(fs::read_dir(&fixture.root).expect("directory").count(), 1);
        let args = vec!["selection".into(), "--config".into(), fixture.config()];
        assert_eq!(
            configuration_path(&definition, &args).expect("public resolver"),
            fixture.path
        );
    }
    #[test]
    fn jsonc_plugin_cli_preserves_comments_and_refuses_preexisting_provider_ownership() {
        let definition = definitions()
            .into_iter()
            .find(|d| !d.reject_existing.is_empty())
            .expect("custom provider plugin");
        let original = b"// synthetic comment\n{\"model\":\"original/model\",\"unknown\":true,}\n";
        let fixture = Fixture::new(original);
        fixture.connect(&definition);
        fixture
            .call(&definition, "disconnect", &[])
            .expect("restore");
        assert_eq!(fixture.bytes(), original);
        let original = br#"{"provider":{"magpie":{"apiKey":"fixture-secret-never-output"}}}"#;
        let fixture = Fixture::new(original);
        let error = fixture
            .call(
                &definition,
                "preview",
                &["--model", "fixture/model-a", "--listen", "127.0.0.1:43197"],
            )
            .expect_err("unowned provider");
        assert!(error.starts_with("config_conflict:"));
        assert!(!error.contains("fixture-secret"));
        assert_eq!(fixture.bytes(), original);
    }
}

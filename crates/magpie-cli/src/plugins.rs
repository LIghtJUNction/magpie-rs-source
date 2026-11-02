//! CLI application composition and owner-aware plugin dispatch.
use magpie_app_plugins::{AppPlugins, FeatureRegistration};
use magpie_core::plugins::{PluginError, PluginInput, PluginKind};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};

mod dispatcher;
pub use dispatcher::MetadataDispatcher;

pub fn features() -> Vec<FeatureRegistration> {
    vec![FeatureRegistration {
        input: PluginInput::bundled("feature.usage", magpie_plugin_usage::MANIFEST),
        commands: vec![("usage".into(), "usage.report".into())],
        pages: vec![("usage".into(), "usage".into())],
        metadata_subscribers: vec![("usage".into(), "request_finished".into())],
    }]
}
pub fn load() -> Result<AppPlugins, String> {
    AppPlugins::load_with_engines(
        &features(),
        &["native-codex/1".into(), "agent-discovery/1".into()],
    )
    .map_err(|_| "plugin_state_error: plugin state could not be loaded safely".into())
}
pub fn metadata_dispatcher(store: &Path) -> MetadataDispatcher {
    let store = store.to_owned();
    MetadataDispatcher::new(
        Arc::new(load),
        vec![dispatcher::SubscriberFactory {
            owner: magpie_plugin_usage::PLUGIN_ID.into(),
            create: Arc::new(move || Arc::new(magpie_plugin_usage::UsagePlugin::new(&store))),
        }],
    )
}
fn static_error(error: PluginError) -> String {
    match error {
        PluginError::Conflict => {
            "plugin_conflict: plugin state changed; reload before saving".into()
        }
        PluginError::Busy => "plugin_busy: another plugin update is in progress".into(),
        _ => "plugin_update_failed: plugin preference could not be saved safely".into(),
    }
}
pub fn run(args: impl Iterator<Item = String>) -> Result<(), String> {
    let args: Vec<_> = args.collect();
    match args.first().map(String::as_str).unwrap_or("list") {
        "list" if args.iter().skip(1).all(|a| a == "--json") && args.len() <= 2 => {
            let app = load()?;
            println!("{}", app.to_json());
            Ok(())
        }
        "enable" | "disable" => toggle(&args),
        "call" => {
            let owner = args
                .get(1)
                .ok_or("plugin_invalid_request: missing plugin ID")?;
            let operation = args
                .get(2)
                .ok_or("plugin_invalid_request: missing plugin operation")?;
            let result = call(owner, operation, &args[3..])?;
            println!(
                "{}",
                json!({"version":magpie_core::VERSION,"plugin_id":owner,"operation":operation,"result":result})
            );
            Ok(())
        }
        "--help" | "-h" => {
            println!(
                "magpie plugins list --json\nmagpie plugins <enable|disable> ID [--if-revision N] [--config ABS_PATH] --json\nmagpie plugins call ID OPERATION [OPTIONS]\nNode/npm and executable plugin hosts are unsupported"
            );
            Ok(())
        }
        _ => Err("plugin_invalid_request: unknown or repeated plugin option".into()),
    }
}
fn toggle(args: &[String]) -> Result<(), String> {
    let owner = args
        .get(1)
        .ok_or("plugin_invalid_request: missing plugin ID")?;
    let enabled = args[0] == "enable";
    let mut revision = None;
    let mut json_output = false;
    let mut config = Vec::new();
    let mut options = args.iter().skip(2);
    while let Some(arg) = options.next() {
        match arg.as_str() {
            "--json" if !json_output => json_output = true,
            "--if-revision" if revision.is_none() => {
                revision = Some(
                    options
                        .next()
                        .and_then(|n| n.parse::<u64>().ok())
                        .ok_or("plugin_invalid_request: invalid expected revision")?,
                );
            }
            "--config" if config.is_empty() && !enabled => {
                config.push(arg.clone());
                config.push(
                    options
                        .next()
                        .ok_or("plugin_invalid_request: missing config path")?
                        .clone(),
                );
            }
            _ => return Err("plugin_invalid_request: unknown or repeated plugin option".into()),
        }
    }
    let mut app = load()?;
    let record = app
        .registry
        .record(owner)
        .ok_or("plugin_not_found: plugin is not installed")?;
    if !enabled && owner == magpie_plugin_agent_codex::PLUGIN_ID {
        let mut targets = magpie_app_plugins::native_targets(owner)?;
        if let Some(path) = config.get(1) {
            targets.push(path.into());
        }
        if let Some(path) = magpie_plugin_agent_codex::agents::codex_config_path() {
            targets.push(path);
        }
        for target in targets {
            match magpie_plugin_agent_codex::codex_transaction::connection_status(&target) {
                Ok(magpie_plugin_agent_codex::codex_transaction::ConnectionStatus::Unmanaged) => {}
                Ok(_) => {
                    return Err(
                        "plugin_in_use: disconnect Codex before disabling its adapter".into(),
                    );
                }
                Err(_) => {
                    return Err(
                        "plugin_in_use: Codex ownership could not be verified safely".into(),
                    );
                }
            }
        }
    } else if !enabled && record.manifest.kind == PluginKind::AgentAdapter {
        let binding = magpie_app_plugins::recovery_binding(owner)
            .map_err(|_| "plugin_recovery_error: Agent recovery metadata is unavailable")?;
        let definition = binding
            .as_ref()
            .map(|b| b.definition.clone())
            .map_or_else(|| app.recovery_agent(owner), Ok)?;
        crate::agent_plugins::ensure_not_in_use(&definition, &config)?;
        if let Some(binding) = binding {
            for target in binding.targets {
                crate::agent_plugins::ensure_not_in_use(
                    &definition,
                    &["--config".into(), target.to_string_lossy().into_owned()],
                )?;
            }
        }
    }
    let withdrawn = app
        .registry
        .set_enabled(owner, enabled, revision.unwrap_or(app.registry.revision()))
        .map_err(static_error)?;
    let mut output = load()?.to_json();
    output["withdrawn"] = json!(withdrawn);
    println!("{output}");
    Ok(())
}
pub fn call(owner: &str, operation: &str, arguments: &[String]) -> Result<Value, String> {
    if arguments.iter().filter(|a| a.as_str() == "--json").count() > 1 {
        return Err("plugin_invalid_request: repeated JSON option".into());
    }
    let app = load()?;
    if owner == magpie_plugin_usage::PLUGIN_ID {
        if !app.active(owner) {
            return Err("plugin_not_active: the Usage plugin is disabled or unavailable".into());
        }
        if operation != "report" {
            return Err("plugin_operation_unsupported: unknown plugin operation".into());
        }
        if !app
            .commands
            .iter()
            .any(|c| c.owner == owner && c.handler == "usage.report")
        {
            return Err("plugin_not_active: Usage command was not registered".into());
        }
        let arguments: Vec<String> = arguments
            .iter()
            .flat_map(|arg| {
                if let Some(limit) = arg.strip_prefix("--limit=").or_else(|| {
                    arg.strip_prefix("--limit")
                        .filter(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
                }) {
                    vec!["--limit".into(), limit.into()]
                } else {
                    vec![arg.clone()]
                }
            })
            .collect();
        return magpie_plugin_usage::command::execute(&arguments)
            .map_err(|_| "plugin_call_failed: Usage report could not be read safely".into());
    }
    if owner == magpie_plugin_agent_codex::PLUGIN_ID {
        if !app.active(owner) && operation != "disconnect" {
            return Err("plugin_not_active: Codex adapter is disabled; explicit disconnect remains available".into());
        }
        let mut args = vec![operation.to_owned()];
        args.extend(arguments.iter().filter(|a| a.as_str() != "--json").cloned());
        if matches!(operation, "connect" | "model") {
            let target = magpie_plugin_agent_codex::command::configuration_path(&args)?;
            magpie_app_plugins::pin_native_target(owner, &target)?;
        }
        return magpie_plugin_agent_codex::command::execute(&args);
    }
    let binding = magpie_app_plugins::recovery_binding(owner)
        .map_err(|_| "plugin_recovery_error: Agent recovery metadata is unavailable")?;
    let definition = if let Some(binding) = &binding {
        binding.definition.clone()
    } else {
        app.agents
            .iter()
            .find(|a| a.owner == owner)
            .map(|a| a.definition.clone())
            .ok_or("plugin_not_active: Agent plugin is disabled or unavailable")?
    };
    if !app.active(owner) && operation != "disconnect" {
        return Err("plugin_not_active: Agent plugin is disabled or unavailable; explicit disconnect remains available".into());
    }
    let mut args = vec![operation.to_owned()];
    args.extend(arguments.iter().filter(|a| a.as_str() != "--json").cloned());
    let target = if matches!(operation, "connect" | "model" | "disconnect") {
        Some(crate::agent_plugins::configuration_path(
            &definition,
            &args,
        )?)
    } else {
        None
    };
    if matches!(operation, "connect" | "model") {
        magpie_app_plugins::pin_recovery(
            owner,
            &definition,
            target
                .as_deref()
                .ok_or("plugin_invalid_request: missing config path")?,
        )?;
    }
    let output = crate::agent_plugins::execute(&definition, &args)?;
    if operation == "disconnect" && binding.is_some() {
        magpie_app_plugins::release_recovery(
            owner,
            target
                .as_deref()
                .ok_or("plugin_invalid_request: missing config path")?,
        )?;
    }
    Ok(output)
}

//! Explicit updates to the model of an already owned Codex connection.
//! CLI output contains no configuration values or recovery file contents.

use magpie_core::codex_transaction::{self, ConnectionStatus};
use std::path::PathBuf;

pub(crate) fn run(mut args: impl Iterator<Item = String>) -> Result<(), String> {
    if args.next().as_deref() != Some("codex") {
        return Err("unsupported agent".into());
    }
    let (mut model, mut config, mut expected_revision) = (None, None, None);
    let (mut apply, mut dry_run, mut json) = (false, false, false);
    while let Some(option) = args.next() {
        match option.as_str() {
            "--model" if model.is_none() => {
                model = Some(args.next().ok_or("--model requires a value")?);
            }
            "--config" if config.is_none() => {
                config = Some(args.next().ok_or("--config requires a path")?);
            }
            "--if-revision" if expected_revision.is_none() => {
                let token = args.next().ok_or("--if-revision requires a token")?;
                if token.len() != 64
                    || !token
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                {
                    return Err("invalid connection revision token".into());
                }
                expected_revision = Some(token);
            }
            "--apply" if !apply => apply = true,
            "--dry-run" if !dry_run => dry_run = true,
            "--json" if !json => json = true,
            _ => return Err("unexpected or duplicate model update argument".into()),
        }
    }
    if apply == dry_run {
        return Err("choose exactly one of --apply or --dry-run".into());
    }
    let path = PathBuf::from(config.ok_or("--config is required for model updates")?);
    let model = model.ok_or("--model is required")?;
    let plan = codex_transaction::prepare_model_update(&path, &model)
        .map_err(|error| format!("config_transaction_error: {error}"))?;
    if expected_revision
        .as_deref()
        .is_some_and(|expected| expected != plan.revision())
    {
        return Err(format!(
            "config_transaction_error: {}",
            codex_transaction::TransactionError::OriginalChanged,
        ));
    }
    let gateway = plan.gateway_addr();
    let fields = plan
        .changed_fields()
        .iter()
        .map(|field| format!("\"{}\"", crate::json_escape(field)))
        .collect::<Vec<_>>()
        .join(",");
    let (status, revision, changed) = if apply {
        // Probe only the already-owned loopback endpoint, never a new endpoint
        // supplied with this update. The core rechecks the preview after IO.
        magpie_service::forward::probe_model(gateway, &model)
            .map_err(|error| format!("gateway_probe_failed: {error}"))?;
        let outcome = codex_transaction::commit_model_update(plan)
            .map_err(|error| format!("config_transaction_error: {error}"))?;
        (outcome.status, outcome.revision, outcome.changed)
    } else {
        (
            ConnectionStatus::Connected,
            plan.revision().to_owned(),
            false,
        )
    };
    let readiness = if apply {
        "listener_and_configured_model_checked"
    } else {
        "not_checked"
    };
    if json {
        println!(
            "{{\"operation\":\"agents.set-model\",\"dry_run\":{},\"writes_performed\":{},\"state\":\"{}\",\"revision\":\"{}\",\"gateway_address\":\"{}\",\"gateway_readiness\":\"{}\",\"provider_readiness\":\"not_probed\",\"agent_execution\":\"not_checked\",\"changed_fields\":[{}]}}",
            dry_run,
            changed,
            status.as_str(),
            crate::json_escape(&revision),
            gateway,
            readiness,
            fields,
        );
    } else {
        println!(
            "Codex model update: {}\nWrites performed: {}\nGateway: {} ({})\nProvider: not probed\nAgent execution: not checked\nChanged fields: [{}]",
            status.as_str(),
            changed,
            gateway,
            readiness,
            fields,
        );
    }
    Ok(())
}

use std::env;
use std::process::ExitCode;

use magpie_core::agents::{self, CodexDiscovery};
use magpie_core::codex_config::{self, CodexConnectPreview, FieldChange};
use magpie_core::paths;

fn main() -> ExitCode {
    let json_errors = env::args().any(|argument| argument == "--json");
    match run(env::args().skip(1)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            let (code, detail) = message
                .split_once(": ")
                .filter(|(code, _)| {
                    code.bytes()
                        .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
                })
                .unwrap_or(("invalid_request", message.as_str()));
            if json_errors {
                eprintln!(
                    "{{\"version\":\"{}\",\"error\":{{\"code\":\"{}\",\"message\":\"{}\"}}}}",
                    json_escape(magpie_core::VERSION),
                    json_escape(code),
                    json_escape(detail),
                );
            } else {
                eprintln!("magpie: {detail}");
            }
            ExitCode::from(error_exit_code(code))
        }
    }
}

fn error_exit_code(code: &str) -> u8 {
    match code {
        "config_conflict" => 4,
        "config_not_a_file"
        | "config_symlink"
        | "config_too_large"
        | "config_invalid_utf8"
        | "config_read_error"
        | "config_parse_error" => 5,
        _ => 2,
    }
}

fn run(mut args: impl Iterator<Item = String>) -> Result<(), String> {
    let Some(command) = args.next() else {
        print_help();
        return Ok(());
    };

    match command.as_str() {
        "--help" | "-h" => {
            print_help();
            Ok(())
        }
        "--version" | "-V" => {
            println!("magpie {}", magpie_core::VERSION);
            Ok(())
        }
        "agents" => agents_command(args),
        "config" => config_command(args),
        "models" => models_command(args),
        "gui" => gui_command(args),
        "serve" => serve_command(args),
        "status" => status_command(args),
        _ => Err("unknown command; try 'magpie --help'".to_owned()),
    }
}

fn agents_command(mut args: impl Iterator<Item = String>) -> Result<(), String> {
    match args.next().as_deref() {
        None => {
            let installed = agents::discover_installed_agents();
            if installed.is_empty() {
                println!("No supported agents found on PATH");
            }
            for agent in installed {
                if agent.id == "codex" {
                    print_agent_status(&agents::discover_codex());
                } else {
                    println!(
                        "{}\n  binary: {}\n  config: not checked",
                        agent.name,
                        agent.executable.display()
                    );
                }
            }
            Ok(())
        }
        Some("--json") => {
            reject_extra_args(args)?;
            let items = agents::discover_installed_agents().iter().map(|agent| {
                if agent.id == "codex" {
                    agent_json(&agents::discover_codex())
                } else {
                    format!("{{\"id\":\"{}\",\"name\":\"{}\",\"installed\":true,\"binary_path\":{},\"config_path\":null,\"config_state\":\"not_checked\"}}", agent.id, agent.name, optional_json_path(Some(&agent.executable)))
                }
            }).collect::<Vec<_>>().join(",");
            println!(
                "{{\"version\":\"{}\",\"items\":[{}]}}",
                json_escape(magpie_core::VERSION),
                items,
            );
            Ok(())
        }
        Some("status") => {
            let Some(agent) = args.next() else {
                return Err("usage: magpie agents status codex [--json]".to_owned());
            };
            if agent != "codex" {
                return Err("unsupported agent".to_owned());
            }
            let json = match args.next().as_deref() {
                None => false,
                Some("--json") => true,
                Some(_) => return Err("unexpected argument".to_owned()),
            };
            reject_extra_args(args)?;

            let discovery = agents::discover_codex();
            if json {
                println!(
                    "{{\"version\":\"{}\",\"agent\":{}}}",
                    json_escape(magpie_core::VERSION),
                    agent_json(&discovery),
                );
            } else {
                print_agent_status(&discovery);
            }
            Ok(())
        }
        Some("connect") => connect_command(args),
        Some(_) => Err("unknown agents command".to_owned()),
    }
}

fn connect_command(mut args: impl Iterator<Item = String>) -> Result<(), String> {
    let Some(agent) = args.next() else {
        return Err(
            "usage: magpie agents connect codex --model local/fixture-a --dry-run [--json]"
                .to_owned(),
        );
    };
    if agent != "codex" {
        return Err("unsupported agent".to_owned());
    }

    let mut model = None;
    let mut dry_run = false;
    let mut json = false;
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--model" => {
                if model.is_some() {
                    return Err("--model may be specified once".to_owned());
                }
                model = Some(
                    args.next()
                        .ok_or_else(|| "--model requires a value".to_owned())?,
                );
            }
            "--dry-run" => dry_run = true,
            "--json" => json = true,
            _ => return Err("unexpected argument".to_owned()),
        }
    }

    if !dry_run {
        return Err("configuration writes are disabled; add --dry-run to preview".to_owned());
    }
    let model = model.ok_or_else(|| "--model is required".to_owned())?;
    let agent_installed = agents::find_codex_executable().is_some();
    let path = agents::codex_config_path()
        .ok_or_else(|| "cannot locate Codex config; set CODEX_HOME".to_owned())?;
    let preview = codex_config::preview_connect(&path, &model)
        .map_err(|error| format!("{}: {error}", error.code()))?;

    if json {
        println!(
            "{{\"version\":\"{}\",\"operation\":\"agents.connect\",\"agent\":\"codex\",\"dry_run\":true,\"writes_performed\":false,\"agent_installed\":{},\"config_path\":{},\"config_exists\":{},\"model\":\"{}\",\"gateway_readiness\":\"not_checked\",\"provider_readiness\":\"not_implemented\",\"changes\":[{}]}}",
            json_escape(magpie_core::VERSION),
            agent_installed,
            optional_json_path(Some(&path)),
            preview.config_exists,
            json_escape(&preview.model),
            changes_json(&preview.changes),
        );
    } else {
        print_connect_preview(agent_installed, &path, &preview);
    }
    Ok(())
}

fn changes_json(changes: &[FieldChange]) -> String {
    changes
        .iter()
        .map(|change| {
            let before = change
                .before
                .as_deref()
                .map(|value| format!("\"{}\"", json_escape(value)))
                .unwrap_or_else(|| "null".to_owned());
            format!(
                "{{\"field\":\"{}\",\"before\":{},\"after\":\"{}\"}}",
                json_escape(change.field),
                before,
                json_escape(&change.after),
            )
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn print_connect_preview(
    agent_installed: bool,
    path: &std::path::Path,
    preview: &CodexConnectPreview,
) {
    println!("Codex connect preview (read-only; no files changed)");
    println!(
        "binary: {}",
        if agent_installed {
            "found"
        } else {
            "not found"
        }
    );
    println!("config: {}", path.display());
    println!(
        "config file: {}",
        if preview.config_exists {
            "exists"
        } else {
            "would be created"
        }
    );
    println!("gateway readiness: not checked");
    println!("provider readiness: not implemented");
    println!("model: {}", preview.model);
    for change in &preview.changes {
        let before = change
            .before
            .as_deref()
            .map(|value| format!("{value:?}"))
            .unwrap_or_else(|| "<unset>".to_owned());
        println!("  {}: {before} -> {:?}", change.field, change.after);
    }
}

fn agent_json(discovery: &CodexDiscovery) -> String {
    let binary_path = optional_json_path(discovery.executable.as_deref());
    let config_path = optional_json_path(discovery.config_path.as_deref());
    format!(
        "{{\"id\":\"codex\",\"name\":\"Codex\",\"installed\":{},\"binary_path\":{},\"config_path\":{},\"config_state\":\"{}\",\"config_error_line\":{},\"model\":{},\"model_provider\":{},\"reasoning_effort\":{}}}",
        discovery.executable.is_some(),
        binary_path,
        config_path,
        discovery.config_state.as_str(),
        discovery
            .config_error_line
            .map(|line| line.to_string())
            .unwrap_or_else(|| "null".to_owned()),
        optional_json_string(discovery.selection.model.as_deref()),
        optional_json_string(discovery.selection.model_provider.as_deref()),
        optional_json_string(discovery.selection.model_reasoning_effort.as_deref()),
    )
}

fn optional_json_string(value: Option<&str>) -> String {
    value
        .map(|value| format!("\"{}\"", json_escape(value)))
        .unwrap_or_else(|| "null".to_owned())
}

fn optional_json_path(path: Option<&std::path::Path>) -> String {
    path.map(|path| format!("\"{}\"", json_escape(&path.to_string_lossy())))
        .unwrap_or_else(|| "null".to_owned())
}

fn print_agent_status(discovery: &CodexDiscovery) {
    println!("Codex");
    match discovery.executable.as_deref() {
        Some(path) => println!("  binary: found ({})", path.display()),
        None => println!("  binary: not found on PATH"),
    }
    match discovery.config_path.as_deref() {
        Some(path) => println!(
            "  config: {} ({})",
            path.display(),
            discovery.config_state.as_str()
        ),
        None => println!("  config: unavailable (no user home or CODEX_HOME)"),
    }
    if let Some(line) = discovery.config_error_line {
        println!("  TOML: invalid near line {line}");
    }
    for (label, value) in [
        ("model", &discovery.selection.model),
        ("provider", &discovery.selection.model_provider),
        ("effort", &discovery.selection.model_reasoning_effort),
    ] {
        if let Some(value) = value {
            println!("  {label}: {value}");
        }
    }
}

fn config_command(mut args: impl Iterator<Item = String>) -> Result<(), String> {
    match args.next().as_deref() {
        Some("path") => {
            let json = match args.next().as_deref() {
                None => false,
                Some("--json") => true,
                Some(_) => return Err("unexpected argument".to_owned()),
            };
            reject_extra_args(args)?;

            let path = paths::config_dir().map_err(|error| error.to_string())?;
            if json {
                println!(
                    "{{\"config_dir\":\"{}\"}}",
                    json_escape(&path.to_string_lossy())
                );
            } else {
                println!("{}", path.display());
            }
            Ok(())
        }
        Some(_) => Err("unknown config command".to_owned()),
        None => Err("usage: magpie config path [--json]".to_owned()),
    }
}

fn status_command(mut args: impl Iterator<Item = String>) -> Result<(), String> {
    let json = match args.next().as_deref() {
        None => false,
        Some("--json") => true,
        Some(_) => return Err("unexpected argument".to_owned()),
    };
    reject_extra_args(args)?;

    let config_dir = paths::config_dir().map_err(|error| error.to_string())?;
    let os = env::consts::OS;
    let arch = env::consts::ARCH;
    let gateway_bind = magpie_service::default_gateway_addr();

    if json {
        println!(
            "{{\"version\":\"{}\",\"gateway\":\"not_checked\",\"gateway_bind\":\"{}\",\"os\":\"{}\",\"arch\":\"{}\",\"config_dir\":\"{}\"}}",
            json_escape(magpie_core::VERSION),
            json_escape(&gateway_bind.to_string()),
            json_escape(os),
            json_escape(arch),
            json_escape(&config_dir.to_string_lossy()),
        );
    } else {
        println!("magpie {}", magpie_core::VERSION);
        println!("gateway: not checked (use GET /health on the listener)");
        println!("gateway bind: {gateway_bind}");
        println!("platform: {os}/{arch}");
        println!("config: {}", config_dir.display());
    }

    Ok(())
}

fn serve_command(mut args: impl Iterator<Item = String>) -> Result<(), String> {
    let mut address = None;
    let mut catalog_path = None;
    while let Some(option) = args.next() {
        match option.as_str() {
            "--listen" if address.is_none() => {
                address = Some(
                    args.next()
                        .ok_or_else(|| "--listen requires a loopback socket address".to_owned())?
                        .parse::<std::net::SocketAddr>()
                        .map_err(|_| "invalid Gateway socket address".to_owned())?,
                );
            }
            "--catalog" if catalog_path.is_none() => {
                catalog_path = Some(
                    args.next()
                        .ok_or_else(|| "--catalog requires a path".to_owned())?,
                );
            }
            _ => {
                return Err(
                    "usage: magpie serve [--listen 127.0.0.1:PORT] [--catalog PATH]".to_owned(),
                );
            }
        }
    }
    let address = address.unwrap_or_else(magpie_service::default_gateway_addr);
    let catalog = load_catalog(catalog_path.as_deref())?;
    let server = magpie_service::GatewayServer::bind_with_catalog(
        address,
        magpie_service::GatewayLimits::default(),
        catalog,
    )
    .map_err(|error| format!("gateway_bind_failed: cannot bind {address}: {error}"))?;
    let address = server
        .local_addr()
        .map_err(|_| "gateway_bind_failed: cannot inspect the local Gateway address".to_owned())?;
    println!("Gateway listening at {address}");
    server
        .run()
        .map_err(|_| "gateway_runtime_error: the local Gateway stopped unexpectedly".to_owned())
}

fn load_catalog(path: Option<&str>) -> Result<magpie_core::catalog::Catalog, String> {
    match path {
        Some(path) => magpie_core::catalog::Catalog::load(std::path::Path::new(path))
            .map_err(|error| format!("catalog_error: {error}")),
        None => Ok(magpie_core::catalog::Catalog::default()),
    }
}

fn models_command(mut args: impl Iterator<Item = String>) -> Result<(), String> {
    let mut path = None;
    let mut json = false;
    while let Some(option) = args.next() {
        match option.as_str() {
            "--catalog" if path.is_none() => {
                path = Some(
                    args.next()
                        .ok_or_else(|| "--catalog requires a path".to_owned())?,
                );
            }
            "--json" if !json => json = true,
            _ => return Err("usage: magpie models [--catalog PATH] [--json]".to_owned()),
        }
    }
    let catalog = load_catalog(path.as_deref())?;
    println!("{}", catalog.models_json());
    Ok(())
}

fn gui_command(mut args: impl Iterator<Item = String>) -> Result<(), String> {
    match args.next().as_deref() {
        None | Some("agents") => reject_extra_args(args)?,
        Some(_) => return Err("usage: magpie gui [agents]".to_owned()),
    }
    let sibling = env::current_exe().ok().and_then(|path| {
        path.parent()
            .map(|parent| parent.join(format!("magpie-desktop{}", env::consts::EXE_SUFFIX)))
    });
    let program = sibling
        .filter(|path| path.is_file())
        .unwrap_or_else(|| std::path::PathBuf::from("magpie-desktop"));
    let status = std::process::Command::new(program)
        .status()
        .map_err(|error| format!("gui_unavailable: install the GPUI desktop package: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("gui_failed: desktop process exited with {status}"))
    }
}

fn reject_extra_args(mut args: impl Iterator<Item = String>) -> Result<(), String> {
    if args.next().is_some() {
        return Err("unexpected argument".to_owned());
    }
    Ok(())
}

fn json_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            character if character <= '\u{1f}' => {
                use std::fmt::Write as _;
                let _ = write!(escaped, "\\u{:04x}", character as u32);
            }
            character => escaped.push(character),
        }
    }
    escaped
}

fn print_help() {
    println!(
        "magpie {}\n\nUsage:\n  magpie <command> [options]\n\nCommands:\n  agents [--json]       Discover Codex and inspect its config path\n  agents status codex   Show Codex discovery status\n  agents connect codex  Preview a reversible config change (requires --dry-run)\n  config path [--json]  Show the configuration directory\n  models [--catalog PATH] [--json]  List configured provider/model IDs\n  serve [--listen ADDR] [--catalog PATH]  Run the loopback Gateway\n  gui [agents]          Open the GPUI desktop application\n  status [--json]       Show version and target platform\n  --help                Show this help\n  --version             Show version",
        magpie_core::VERSION
    );
}

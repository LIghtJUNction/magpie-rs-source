//! Provider commands share the exact store and credential transaction used by GPUI.

use std::{io::Read, path::PathBuf};

use magpie_core::{
    credentials::{self, SaveMode, SecretChange},
    provider_store::{Provider, ProviderProtocol, ProviderStore},
};

const HELP: &str = "Usage:
  magpie providers [list] [--store PATH] [--json]
  magpie providers add ID --name NAME --url BASE --protocol PROTOCOL --model ID...
  magpie providers edit ID [--name NAME] [--url BASE] [--model ID... | --clear-models]
  magpie providers enable|disable ID
  magpie providers refresh ID
  magpie providers remove ID --yes

Options:
  --store PATH           Use an isolated Provider store (default: Magpie config/providers.toml)
  --protocol PROTOCOL    openai_chat, openai_responses, anthropic_messages, gemini
  --key-stdin            Read a replacement API key from stdin into the system keyring
  --key-env NAME         Use an environment variable instead of a stored key
  --clear-key            Explicitly remove saved credential references
  --disabled             Save without enabling the Provider
  --if-revision NUMBER   Reject an update if the stored revision changed
  --json                 Emit public JSON; credentials are never printed

Saving validates configuration without contacting the Provider.
Key input is never accepted as a command-line argument. A blank edit keeps its existing key.
New settings are read by the running Gateway on the next request.";

#[derive(Default)]
struct Options {
    store: Option<PathBuf>,
    name: Option<String>,
    url: Option<String>,
    protocol: Option<ProviderProtocol>,
    models: Vec<String>,
    clear_models: bool,
    key_env: Option<String>,
    key_stdin: bool,
    clear_key: bool,
    disabled: bool,
    revision: Option<u64>,
    yes: bool,
    json: bool,
}

pub fn run(args: impl Iterator<Item = String>) -> Result<(), String> {
    let arguments: Vec<String> = args.collect();
    if arguments.iter().any(|arg| arg == "--help" || arg == "-h") {
        println!("{HELP}");
        return Ok(());
    }
    let mut args = arguments.into_iter().peekable();
    let command = if args.peek().is_some_and(|arg| !arg.starts_with('-')) {
        args.next().unwrap_or_default()
    } else {
        "list".to_owned()
    };
    if ![
        "list", "add", "edit", "enable", "disable", "remove", "refresh",
    ]
    .contains(&command.as_str())
    {
        return Err(
            "provider_error: unknown Provider command; try 'magpie providers --help'".into(),
        );
    }
    let id = if command != "list" {
        Some(
            args.next()
                .filter(|id| !id.starts_with('-'))
                .ok_or("provider_error: a Provider ID is required")?,
        )
    } else {
        None
    };
    let options = parse(args)?;
    let edit = command == "add" || command == "edit";
    if !edit
        && (options.name.is_some()
            || options.url.is_some()
            || options.protocol.is_some()
            || !options.models.is_empty()
            || options.clear_models
            || options.key_env.is_some()
            || options.key_stdin
            || options.clear_key
            || options.disabled)
    {
        return Err("provider_error: configuration options require add or edit".into());
    }
    if command != "remove" && options.yes {
        return Err("provider_error: --yes is only used with remove".into());
    }
    let path = match options.store {
        Some(ref path) => path.clone(),
        None => ProviderStore::default_path().map_err(error)?,
    };
    let store = ProviderStore::load(&path).map_err(error)?;
    let revision = options.revision.unwrap_or(store.revision());
    if revision != store.revision() {
        return Err("provider_conflict: Provider configuration changed; reload and retry".into());
    }
    let mut cleanup_pending = false;
    let result = match command.as_str() {
        "list" => store,
        "refresh" => {
            let id = id.as_deref().unwrap_or_default();
            let store = magpie_service::provider_catalog::refresh_models(&path, id, revision)
                .map_err(|error| format!("provider_refresh_failed: {error}"))?;
            if options.json {
                println!(
                    "{{\"version\":{},\"operation\":\"providers.refresh\",\"provider_id\":{},\"revision\":{}}}",
                    quote(magpie_core::VERSION),
                    quote(id),
                    store.revision()
                );
                return Ok(());
            }
            store
        }
        "enable" | "disable" => ProviderStore::set_enabled(
            &path,
            id.as_deref().unwrap_or_default(),
            command == "enable",
            revision,
        )
        .map_err(error)?,
        "remove" => {
            if !options.yes {
                return Err(
                    "provider_error: removal requires --yes; use disable to preserve configuration"
                        .into(),
                );
            }
            let outcome =
                credentials::remove_provider(&path, id.as_deref().unwrap_or_default(), revision)
                    .map_err(error)?;
            cleanup_pending = outcome.cleanup_pending;
            outcome.store
        }
        "add" | "edit" => {
            let id = id.unwrap_or_default();
            let mut provider = if command == "edit" {
                store
                    .provider(&id)
                    .cloned()
                    .ok_or("provider_error: Provider was not found")?
            } else {
                Provider {
                    id,
                    name: String::new(),
                    base_url: String::new(),
                    protocol: ProviderProtocol::OpenAiChat,
                    api_key_env: None,
                    credential_ref: None,
                    models: Vec::new(),
                    enabled: true,
                }
            };
            if let Some(name) = options.name {
                provider.name = name;
            }
            if let Some(url) = options.url {
                provider.base_url = url;
            }
            if let Some(protocol) = options.protocol {
                provider.protocol = protocol;
            }
            if !options.models.is_empty() || options.clear_models {
                provider.models = options.models;
            }
            if options.disabled {
                provider.enabled = false;
            }
            // Choosing a new credential source is explicit. Normal edits keep
            // the previous key or environment reference without reading it.
            let change = if options.key_stdin {
                SecretChange::Replace(read_key()?)
            } else if let Some(name) = options.key_env {
                provider.api_key_env = Some(name);
                SecretChange::Clear
            } else if options.clear_key {
                provider.api_key_env = None;
                SecretChange::Clear
            } else {
                SecretChange::Keep
            };
            let outcome = credentials::save_provider(
                &path,
                provider,
                revision,
                change,
                if command == "add" {
                    SaveMode::Create
                } else {
                    SaveMode::Update
                },
            )
            .map_err(error)?;
            cleanup_pending = outcome.cleanup_pending;
            outcome.store
        }
        _ => unreachable!(),
    };
    if options.json {
        println!("{}", public_json(&result, cleanup_pending));
    } else {
        print_list(&result);
        if cleanup_pending {
            eprintln!(
                "Provider saved; an unused credential could not be removed from the system keyring."
            );
        }
    }
    Ok(())
}

fn parse(mut args: impl Iterator<Item = String>) -> Result<Options, String> {
    let mut options = Options::default();
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--store" if options.store.is_none() => options.store = Some(value(&mut args)?.into()),
            "--name" if options.name.is_none() => options.name = Some(value(&mut args)?),
            "--url" if options.url.is_none() => options.url = Some(value(&mut args)?),
            "--protocol" if options.protocol.is_none() => options.protocol = Some(ProviderProtocol::parse(&value(&mut args)?).map_err(error)?),
            "--model" if !options.clear_models => options.models.push(value(&mut args)?),
            "--clear-models" if !options.clear_models && options.models.is_empty() => options.clear_models = true,
            "--key-env" if options.key_env.is_none() && !options.key_stdin && !options.clear_key => options.key_env = Some(value(&mut args)?),
            "--key-stdin" if !options.key_stdin && options.key_env.is_none() && !options.clear_key => options.key_stdin = true,
            "--clear-key" if !options.clear_key && !options.key_stdin && options.key_env.is_none() => options.clear_key = true,
            "--disabled" if !options.disabled => options.disabled = true,
            "--if-revision" if options.revision.is_none() => options.revision = Some(value(&mut args)?.parse().map_err(|_| "provider_error: revision must be an unsigned integer")?),
            "--yes" if !options.yes => options.yes = true,
            "--json" if !options.json => options.json = true,
            _ => return Err("provider_error: unknown, repeated or conflicting option; try 'magpie providers --help'".into()),
        }
    }
    Ok(options)
}

fn value(args: &mut impl Iterator<Item = String>) -> Result<String, String> {
    args.next()
        .ok_or_else(|| "provider_error: option requires a value".into())
}

fn read_key() -> Result<String, String> {
    let mut key = zeroize::Zeroizing::new(String::new());
    std::io::stdin()
        .lock()
        .take(8195)
        .read_to_string(&mut key)
        .map_err(|_| "provider_error: cannot read a UTF-8 API key from stdin")?;
    if key.ends_with('\n') {
        key.pop();
        if key.ends_with('\r') {
            key.pop();
        }
    }
    if key.is_empty() || key.len() > 8192 || !key.bytes().all(|byte| byte.is_ascii_graphic()) {
        return Err(
            "provider_error: API key must be 1–8192 visible ASCII characters without spaces".into(),
        );
    }
    Ok(std::mem::take(&mut *key))
}

fn error(error: impl std::fmt::Display) -> String {
    format!("provider_error: {error}")
}

fn quote(value: &str) -> String {
    format!("\"{}\"", crate::json_escape(value))
}

pub fn public_json(store: &ProviderStore, cleanup_pending: bool) -> String {
    let items = store.providers().iter().map(|provider| {
        let models = provider.models.iter().map(|model| quote(model)).collect::<Vec<_>>().join(",");
        let source = if provider.credential_ref.is_some() { "system_keyring" } else if provider.api_key_env.is_some() { "environment" } else { "none" };
        format!("{{\"id\":{},\"name\":{},\"base_url\":{},\"protocol\":{},\"enabled\":{},\"models\":[{}],\"credential_source\":{},\"api_key_env\":{}}}",
            quote(&provider.id), quote(&provider.name), quote(&provider.base_url), quote(provider.protocol.as_str()), provider.enabled, models, quote(source), provider.api_key_env.as_deref().map_or("null".into(), quote))
    }).collect::<Vec<_>>().join(",");
    format!(
        "{{\"version\":{},\"revision\":{},\"providers\":[{}],\"credential_cleanup_pending\":{}}}",
        quote(magpie_core::VERSION),
        store.revision(),
        items,
        cleanup_pending
    )
}

pub fn models_json(store: &ProviderStore) -> String {
    let mut models = Vec::new();
    for provider in store.providers().iter().filter(|provider| provider.enabled) {
        for model in &provider.models {
            models.push(format!("{{\"id\":{},\"object\":\"model\",\"created\":0,\"owned_by\":{},\"name\":{},\"provider_name\":{},\"magpie\":{{\"source\":\"configured\",\"provider_health\":\"not_probed\"}}}}",
                quote(&format!("{}/{model}", provider.id)), quote(&provider.id), quote(model), quote(&provider.name)));
        }
    }
    format!("{{\"object\":\"list\",\"data\":[{}]}}", models.join(","))
}

fn print_list(store: &ProviderStore) {
    if store.providers().is_empty() {
        println!("No Providers configured. Add one with 'magpie providers add --help'.");
        return;
    }
    for provider in store.providers() {
        println!(
            "{}  {}  {}  {}  {} models",
            provider.id,
            if provider.enabled { "on " } else { "off" },
            provider.name,
            provider.base_url,
            provider.models.len()
        );
    }
}

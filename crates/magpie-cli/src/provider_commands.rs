//! Additive Provider commands. No credentials are accepted in argv or JSON.
//! The whole returned snapshot comes from the same core store as the Gateway.

use magpie_core::providers::{
    Protocol, Provider, ProviderKey, ProviderMutation, ProviderSnapshot, ProviderStore,
};
use serde_json::{Map, Value};
use std::{collections::BTreeSet, io::Read, path::PathBuf};

const INPUT_LIMIT: usize = 128 * 1024;
const OUTPUT_LIMIT: usize = 4 * 1024 * 1024;

/// The additive Gateway inspection has its own envelope; legacy inspect is unchanged.
pub fn inspect_gateway(mut args: impl Iterator<Item = String>) -> Result<(), String> {
    let fail = |code: &str| format!("{code}: Provider operation failed.");
    let (mut store, mut address, mut json) = (None, None, false);
    while let Some(option) = args.next() {
        match option.as_str() {
            "--store" if store.is_none() => {
                store = Some(PathBuf::from(
                    args.next()
                        .ok_or_else(|| fail("provider_inspect_argument_invalid"))?,
                ));
            }
            "--listen" if address.is_none() => {
                address = Some(
                    args.next()
                        .ok_or_else(|| fail("provider_inspect_argument_invalid"))?
                        .parse::<std::net::SocketAddr>()
                        .map_err(|_| fail("provider_inspect_argument_invalid"))?,
                );
            }
            "--json" if !json => json = true,
            _ => return Err(fail("provider_inspect_argument_invalid")),
        }
    }
    let store = store
        .filter(|path| path.is_absolute())
        .ok_or_else(|| fail("provider_inspect_argument_invalid"))?;
    let address = address.ok_or_else(|| fail("provider_inspect_argument_invalid"))?;
    if !json {
        return Err(fail("provider_inspect_argument_invalid"));
    }
    let output =
        magpie_service::provider_gateway::inspect_gateway(address, &store).map_err(fail)?;
    if output.len() > OUTPUT_LIMIT {
        return Err(fail("provider_gateway_invalid_response"));
    }
    println!("{output}");
    Ok(())
}

pub fn run(args: impl Iterator<Item = String>) -> Result<(), String> {
    let args = Arguments::parse(args)?;
    let store = ProviderStore::new(args.store.clone());
    match args.command.as_str() {
        "list" => {
            args.no_payload_or_expect()?;
            output(&store.load().map_err(core_error)?, None)
        }
        "show" => {
            args.no_payload_or_expect()?;
            let snapshot = store.load().map_err(core_error)?;
            if snapshot
                .provider(args.id()?)
                .is_none_or(|provider| provider.deleted)
            {
                return Err(error("provider_not_found"));
            }
            output(&snapshot, None)
        }
        "recover" => {
            args.no_payload_or_expect()?;
            output(&store.recover().map_err(core_error)?, None)
        }
        "refresh" | "test" => {
            args.no_payload()?;
            let result = if args.command == "refresh" {
                magpie_service::provider_gateway::refresh(&store, args.expect()?, args.id()?)
            } else {
                magpie_service::provider_gateway::test(&store, args.expect()?, args.id()?)
            }
            .map_err(error)?;
            output(&result.snapshot, Some(result.report.to_json()))
        }
        "remove" => {
            args.no_payload()?;
            let snapshot = store
                .apply(
                    args.expect()?,
                    ProviderMutation::Remove {
                        id: args.id()?.to_owned(),
                    },
                )
                .map_err(core_error)?;
            output(&snapshot, None)
        }
        _ => {
            let payload = args.payload()?;
            let mutation = mutation(&args.command, args.id()?, &payload)?;
            let snapshot = store.apply(args.expect()?, mutation).map_err(core_error)?;
            output(&snapshot, None)
        }
    }
}

struct Arguments {
    command: String,
    id: Option<String>,
    store: PathBuf,
    expected: Option<String>,
    data: Option<String>,
    stdin: bool,
}

impl Arguments {
    fn parse(mut args: impl Iterator<Item = String>) -> Result<Self, String> {
        let command = args
            .next()
            .filter(|command| {
                matches!(
                    command.as_str(),
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
                )
            })
            .ok_or_else(|| error("provider_command_invalid"))?;
        let needs_id = !matches!(command.as_str(), "list" | "recover");
        let id = if needs_id {
            Some(
                args.next()
                    .filter(|id| !id.starts_with('-') && id.len() <= 128)
                    .ok_or_else(|| error("provider_id_required"))?,
            )
        } else {
            None
        };
        let (mut store, mut expected, mut data) = (None, None, None);
        let (mut stdin, mut json) = (false, false);
        while let Some(option) = args.next() {
            match option.as_str() {
                "--store" if store.is_none() => {
                    store = Some(PathBuf::from(
                        args.next()
                            .ok_or_else(|| error("provider_store_required"))?,
                    ));
                }
                "--expect" if expected.is_none() => {
                    expected = Some(
                        args.next()
                            .filter(|token| token.len() <= 128)
                            .ok_or_else(|| error("provider_expected_token_required"))?,
                    );
                }
                "--data" if data.is_none() && !stdin => {
                    data = Some(
                        args.next()
                            .filter(|data| data.len() <= INPUT_LIMIT)
                            .ok_or_else(|| error("provider_input_limit"))?,
                    );
                }
                "--stdin" if !stdin && data.is_none() => stdin = true,
                "--json" if !json => json = true,
                _ => return Err(error("provider_argument_invalid")),
            }
        }
        let store = match store {
            Some(path) => path,
            None => ProviderStore::default_path().map_err(core_error)?,
        };
        if !store.is_absolute() {
            return Err(error("provider_store_absolute_path_required"));
        }
        Ok(Self {
            command,
            id,
            store,
            expected,
            data,
            stdin,
        })
    }

    fn id(&self) -> Result<&str, String> {
        self.id
            .as_deref()
            .ok_or_else(|| error("provider_id_required"))
    }

    fn expect(&self) -> Result<&str, String> {
        self.expected
            .as_deref()
            .ok_or_else(|| error("provider_expected_token_required"))
    }

    fn no_payload(&self) -> Result<(), String> {
        if self.data.is_some() || self.stdin {
            return Err(error("provider_argument_invalid"));
        }
        Ok(())
    }

    fn no_payload_or_expect(&self) -> Result<(), String> {
        self.no_payload()?;
        if self.expected.is_some() {
            return Err(error("provider_argument_invalid"));
        }
        Ok(())
    }

    fn payload(&self) -> Result<Map<String, Value>, String> {
        let bytes = if self.stdin {
            let mut bytes = Vec::new();
            std::io::stdin()
                .take(INPUT_LIMIT as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| error("provider_input_unreadable"))?;
            if bytes.len() > INPUT_LIMIT {
                return Err(error("provider_input_limit"));
            }
            bytes
        } else {
            self.data
                .as_ref()
                .ok_or_else(|| error("provider_input_required"))?
                .as_bytes()
                .to_vec()
        };
        strict_object(&bytes)
    }
}

fn mutation(
    command: &str,
    id: &str,
    data: &Map<String, Value>,
) -> Result<ProviderMutation, String> {
    match command {
        "add" => {
            only(
                data,
                &[
                    "name",
                    "base_url",
                    "protocol",
                    "enabled",
                    "allow_insecure_http",
                    "keys",
                    "models",
                    "selected_models",
                    "auto_include",
                ],
            )?;
            let keys = match data.get("keys") {
                None => Vec::new(),
                Some(Value::Array(keys)) if keys.len() <= 32 => keys
                    .iter()
                    .map(|key| {
                        parse_key(
                            key.as_object()
                                .ok_or_else(|| error("provider_input_invalid"))?,
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()?,
                _ => return Err(error("provider_input_invalid")),
            };
            let models = optional_strings(data, "models")?.unwrap_or_default();
            let selected_models = optional_strings(data, "selected_models")?.unwrap_or_default();
            Ok(ProviderMutation::Add(Provider {
                id: id.to_owned(),
                name: string(data, "name")?.to_owned(),
                base_url: string(data, "base_url")?.to_owned(),
                protocol: Protocol::parse(string(data, "protocol")?).map_err(core_error)?,
                enabled: optional_bool(data, "enabled")?.unwrap_or(true),
                deleted: false,
                allow_insecure_http: optional_bool(data, "allow_insecure_http")?.unwrap_or(false),
                keys,
                models,
                selected_models,
                auto_include: optional_bool(data, "auto_include")?.unwrap_or(true),
                catalog_checked_at: None,
                catalog_error: None,
            }))
        }
        "edit" => {
            only(
                data,
                &[
                    "name",
                    "base_url",
                    "protocol",
                    "enabled",
                    "allow_insecure_http",
                ],
            )?;
            if data.is_empty() {
                return Err(error("provider_input_required"));
            }
            Ok(ProviderMutation::Edit {
                id: id.to_owned(),
                name: optional_string(data, "name")?,
                base_url: optional_string(data, "base_url")?,
                protocol: optional_string(data, "protocol")?
                    .map(|protocol| Protocol::parse(&protocol).map_err(core_error))
                    .transpose()?,
                enabled: optional_bool(data, "enabled")?,
                allow_insecure_http: optional_bool(data, "allow_insecure_http")?,
            })
        }
        "key-put" => Ok(ProviderMutation::PutKey {
            provider_id: id.to_owned(),
            key: parse_key(data)?,
        }),
        "key-remove" => {
            only(data, &["key_id"])?;
            Ok(ProviderMutation::RemoveKey {
                provider_id: id.to_owned(),
                key_id: string(data, "key_id")?.to_owned(),
            })
        }
        "key-order" => {
            only(data, &["key_ids"])?;
            Ok(ProviderMutation::ReorderKeys {
                provider_id: id.to_owned(),
                key_ids: strings(data, "key_ids")?,
            })
        }
        "models" => {
            only(data, &["models", "selected_models", "auto_include"])?;
            Ok(ProviderMutation::Models {
                provider_id: id.to_owned(),
                models: strings(data, "models")?,
                selected_models: strings(data, "selected_models")?,
                auto_include: data
                    .get("auto_include")
                    .and_then(Value::as_bool)
                    .ok_or_else(|| error("provider_input_invalid"))?,
            })
        }
        _ => Err(error("provider_command_invalid")),
    }
}

fn parse_key(data: &Map<String, Value>) -> Result<ProviderKey, String> {
    only(data, &["id", "name", "env", "enabled"])?;
    Ok(ProviderKey {
        id: string(data, "id")?.to_owned(),
        name: string(data, "name")?.to_owned(),
        env: string(data, "env")?.to_owned(),
        enabled: optional_bool(data, "enabled")?.unwrap_or(true),
        deleted: false,
    })
}

fn only(data: &Map<String, Value>, allowed: &[&str]) -> Result<(), String> {
    if data.keys().any(|key| !allowed.contains(&key.as_str())) {
        // Unknown names may themselves contain secret material. Do not echo them.
        return Err(error("provider_input_unknown_field"));
    }
    Ok(())
}

fn string<'a>(data: &'a Map<String, Value>, key: &str) -> Result<&'a str, String> {
    data.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| error("provider_input_invalid"))
}

fn optional_string(data: &Map<String, Value>, key: &str) -> Result<Option<String>, String> {
    data.get(key)
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| error("provider_input_invalid"))
        })
        .transpose()
}

fn optional_bool(data: &Map<String, Value>, key: &str) -> Result<Option<bool>, String> {
    data.get(key)
        .map(|value| {
            value
                .as_bool()
                .ok_or_else(|| error("provider_input_invalid"))
        })
        .transpose()
}

fn optional_strings(data: &Map<String, Value>, key: &str) -> Result<Option<Vec<String>>, String> {
    if data.contains_key(key) {
        strings(data, key).map(Some)
    } else {
        Ok(None)
    }
}

fn strings(data: &Map<String, Value>, key: &str) -> Result<Vec<String>, String> {
    data.get(key)
        .and_then(Value::as_array)
        .filter(|items| items.len() <= 512)
        .ok_or_else(|| error("provider_input_invalid"))?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| error("provider_input_invalid"))
        })
        .collect()
}

fn strict_object(bytes: &[u8]) -> Result<Map<String, Value>, String> {
    if bytes.len() > INPUT_LIMIT {
        return Err(error("provider_input_limit"));
    }
    let value: Value =
        serde_json::from_slice(bytes).map_err(|_| error("provider_input_invalid_json"))?;
    reject_duplicate_keys(bytes)?;
    value
        .as_object()
        .cloned()
        .ok_or_else(|| error("provider_input_invalid"))
}

/// serde_json has already validated the grammar. Inspect JSON string tokens at
/// each object depth so repeated names cannot silently become last-key-wins.
fn reject_duplicate_keys(bytes: &[u8]) -> Result<(), String> {
    let mut stack: Vec<Option<BTreeSet<String>>> = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'{' => stack.push(Some(BTreeSet::new())),
            b'[' => stack.push(None),
            b'}' | b']' => {
                stack.pop();
            }
            b'"' => {
                let start = index;
                index += 1;
                while index < bytes.len() {
                    if bytes[index] == b'\\' {
                        index += 2;
                    } else if bytes[index] == b'"' {
                        break;
                    } else {
                        index += 1;
                    }
                }
                let mut next = index + 1;
                while bytes.get(next).is_some_and(u8::is_ascii_whitespace) {
                    next += 1;
                }
                if bytes.get(next) == Some(&b':') {
                    let name: String = serde_json::from_slice(&bytes[start..=index])
                        .map_err(|_| error("provider_input_invalid_json"))?;
                    if let Some(Some(names)) = stack.last_mut() {
                        if !names.insert(name) {
                            return Err(error("provider_input_duplicate_field"));
                        }
                    }
                }
            }
            _ => {}
        }
        index += 1;
    }
    Ok(())
}

fn output(snapshot: &ProviderSnapshot, operation: Option<Value>) -> Result<(), String> {
    let mut value: Value =
        serde_json::from_str(&snapshot.to_json()).map_err(|_| error("provider_output_invalid"))?;
    value["version"] = Value::String(magpie_core::VERSION.to_owned());
    if let Some(operation) = operation {
        value["operation"] = operation;
    }
    let encoded = value.to_string();
    if encoded.len() > OUTPUT_LIMIT {
        return Err(error("provider_output_limit"));
    }
    println!("{encoded}");
    Ok(())
}

fn core_error(error_value: magpie_core::providers::ProviderError) -> String {
    error(error_value.code())
}

fn error(code: &str) -> String {
    format!("{code}: {code}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_keys_are_rejected_including_escaped_nested_names() {
        for source in [
            r#"{"name":"a","name":"b"}"#,
            r#"{"name":"a","n\u0061me":"b"}"#,
            r#"{"keys":[{"env":"A","env":"B"}]}"#,
        ] {
            assert!(strict_object(source.as_bytes()).is_err());
        }
        assert!(strict_object(br#"{"keys":[{"env":"A"},{"env":"B"}],"name":"x\"{y}"}"#).is_ok());
    }

    #[test]
    fn secret_fields_and_protocol_errors_do_not_echo_input() {
        for source in [
            r#"{"fixture_private_field":"fixture_private_value"}"#,
            r#"{"name":"a","base_url":"https://example.com/v1","protocol":"fixture_private_value"}"#,
        ] {
            let data = strict_object(source.as_bytes()).unwrap();
            let failure = mutation("add", "fixture", &data).unwrap_err();
            assert!(!failure.contains("fixture_private"));
        }
        let data = strict_object(
            br#"{"id":"key","name":"a","env":"ENV","secret":"fixture_private_value"}"#,
        )
        .unwrap();
        let failure = mutation("key-put", "fixture", &data).unwrap_err();
        assert!(!failure.contains("fixture_private"));
    }

    #[test]
    fn metadata_edit_keeps_unspecified_fields_absent() {
        let data = strict_object(br#"{"enabled":false}"#).unwrap();
        assert!(matches!(
            mutation("edit", "fixture", &data).unwrap(),
            ProviderMutation::Edit {
                enabled: Some(false),
                name: None,
                base_url: None,
                protocol: None,
                allow_insecure_http: None,
                ..
            }
        ));
    }
}

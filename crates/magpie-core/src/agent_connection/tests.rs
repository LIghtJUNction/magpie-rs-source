use super::*;
use crate::test_support::{self, Action, Point};
use std::fs;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt, symlink};
use std::sync::atomic::{AtomicU64, Ordering};
static SEQUENCE: AtomicU64 = AtomicU64::new(0);
pub(super) struct Fixture {
    pub(super) root: PathBuf,
    pub(super) path: PathBuf,
}
impl Fixture {
    pub(super) fn new(bytes: &[u8]) -> Self {
        let root = std::env::temp_dir().join(format!(
            "magpie-declarative-fixture-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .expect("private fixture directory");
        let path = root.join("config.jsonc");
        fs::write(&path, bytes).expect("synthetic config");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("private fixture");
        Self { root, path }
    }
    pub(super) fn bytes(&self) -> Vec<u8> {
        fs::read(&self.path).expect("fixture bytes")
    }
    fn private(&self) {
        let state = self.root.join(".config.jsonc.magpie-agent-state");
        assert_eq!(fs::metadata(&state).expect("state").mode() & 0o777, 0o700);
        for entry in fs::read_dir(state).expect("state entries") {
            let m = entry.expect("entry").metadata().expect("metadata");
            assert_eq!(m.mode() & 0o777, 0o600);
            assert!(m.is_file());
        }
        assert_eq!(
            fs::metadata(&self.path).expect("config").mode() & 0o777,
            0o600
        );
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).expect("remove isolated fixture");
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
pub(super) fn gateway() -> SocketAddr {
    "127.0.0.1:41327".parse().expect("loopback")
}
pub(super) fn connect(def: &AgentDefinition, f: &Fixture, model: &str) {
    commit_connect(prepare_connect(def, &f.path, model, gateway()).expect("preview"))
        .expect("connect");
}
#[test]
fn preview_writes_nothing_and_plans_redact_original_secrets() {
    for def in definitions() {
        let f = Fixture::new(
            br#"{ "model": "original/model", "unrelated": "fixture-secret-sentinel" }"#,
        );
        let plan = prepare_connect(&def, &f.path, "fixture/model-a", gateway()).expect("preview");
        assert!(!plan.revision().is_empty());
        assert!(!plan.changed_fields().is_empty());
        assert!(!format!("{plan:?}").contains("fixture-secret-sentinel"));
        assert_eq!(fs::read_dir(&f.root).expect("directory").count(), 1);
        assert!(
            String::from_utf8(f.bytes())
                .expect("utf8")
                .contains("fixture-secret-sentinel")
        );
    }
}
#[test]
fn plugin_connect_switch_disconnect_restores_first_original_bytes() {
    for def in definitions() {
        let original=b"{\n  \"model\" : \"old/model\",\n  \"env\": {\"UNKNOWN\": \"fixture-only\"},\n  \"unknown\": {\"array\":[1,2,3], \"future\":true}\n}\n";
        let f = Fixture::new(original);
        connect(&def, &f, "fixture/model-a");
        assert_eq!(
            read_selection(&def, &f.path).expect("selection"),
            Selection {
                model: Some("fixture/model-a".into()),
                status: ConnectionStatus::Connected
            }
        );
        let plan = prepare_model_update(&def, &f.path, "fixture/model-b").expect("preview model");
        commit_model_update(plan).expect("switch model");
        assert_eq!(
            read_selection(&def, &f.path)
                .expect("selection")
                .model
                .as_deref(),
            Some("fixture/model-b")
        );
        let output = disconnect(&def, &f.path).expect("disconnect");
        assert_eq!(output.preserved_fields, 0);
        assert!(!output.config_removed);
        assert_eq!(f.bytes(), original);
        f.private();
        assert_eq!(
            read_selection(&def, &f.path).expect("selection").status,
            ConnectionStatus::Unmanaged
        );
    }
}
#[test]
fn unrelated_edits_before_model_update_survive_disconnect() {
    for def in definitions() {
        let f = Fixture::new(br#"{"model":"old/model","unknown":"old"}"#);
        connect(&def, &f, "fixture/model-a");
        let changed = String::from_utf8(f.bytes())
            .expect("utf8")
            .replace("\"unknown\":\"old\"", "\"unknown\":\"external\"");
        fs::write(&f.path, changed).expect("external edit");
        commit_model_update(
            prepare_model_update(&def, &f.path, "fixture/model-b").expect("preview"),
        )
        .expect("update");
        disconnect(&def, &f.path).expect("disconnect");
        let current = jsonc::parse(&f.bytes(), def.format).expect("config").value;
        assert_eq!(current["unknown"], "external");
        assert_eq!(current["model"], "old/model");
    }
}
#[test]
fn old_preview_cannot_apply_after_external_or_ownership_revision_changes() {
    for def in definitions() {
        let f = Fixture::new(b"{}");
        let p = prepare_connect(&def, &f.path, "fixture/model-a", gateway()).expect("preview");
        fs::write(&f.path, b"{\"outside\":true}").expect("external edit");
        assert_eq!(commit_connect(p), Err(Error::OriginalChanged));
        connect(&def, &f, "fixture/model-a");
        let p = prepare_model_update(&def, &f.path, "fixture/model-b").expect("preview");
        let other = prepare_model_update(&def, &f.path, "fixture/model-c").expect("other preview");
        assert_eq!(p.revision(), other.revision());
        commit_model_update(other).expect("other writer");
        assert_eq!(commit_model_update(p), Err(Error::OriginalChanged));
        assert_eq!(
            read_selection(&def, &f.path)
                .expect("selection")
                .model
                .as_deref(),
            Some("fixture/model-c")
        );
        disconnect(&def, &f.path).expect("disconnect");
    }
}
#[test]
fn drift_restores_only_matching_owned_fields_and_preserves_comments() {
    for def in definitions() {
        let f = Fixture::new(b"{\"model\":\"old/model\",\"outside\":1}");
        connect(&def, &f, "fixture/model-a");
        let current = jsonc::set(
            &f.bytes(),
            def.format,
            &["model".into()],
            Some(&serde_json::json!("external/model")),
        )
        .expect("edit");
        let mut current = jsonc::set(
            &current,
            def.format,
            &["outside".into()],
            Some(&serde_json::json!(2)),
        )
        .expect("edit");
        if def.format == ConfigFormat::Jsonc {
            current.splice(1..1, b"\n// external comment must remain\n".iter().copied());
        }
        fs::write(&f.path, &current).expect("external edit");
        assert_eq!(
            read_selection(&def, &f.path).expect("selection").status,
            ConnectionStatus::Drifted
        );
        assert!(matches!(
            prepare_model_update(&def, &f.path, "fixture/model-b"),
            Err(Error::OriginalChanged)
        ));
        let result = disconnect(&def, &f.path).expect("disconnect");
        assert_eq!(result.preserved_fields, 1);
        let after = f.bytes();
        let parsed = jsonc::parse(&after, def.format)
            .expect("restored config")
            .value;
        assert_eq!(parsed["model"], "external/model");
        assert_eq!(parsed["outside"], 2);
        if def.format == ConfigFormat::Jsonc {
            assert!(
                String::from_utf8(after)
                    .expect("utf8")
                    .contains("// external comment must remain")
            );
        }
    }
}
#[test]
fn preexisting_unowned_provider_is_refused_and_nested_external_fields_survive() {
    let def = definitions()
        .into_iter()
        .find(|d| !d.reject_existing.is_empty())
        .expect("provider fixture");
    let f = Fixture::new(br#"{"provider":{"magpie":{"options":{"apiKey":"fixture-user-key"}}}}"#);
    assert!(matches!(
        prepare_connect(&def, &f.path, "fixture/model-a", gateway()),
        Err(Error::UnknownProvider)
    ));
    let f = Fixture::new(b"{}");
    connect(&def, &f, "fixture/model-a");
    let changed = jsonc::set(
        &f.bytes(),
        def.format,
        &[
            "provider".into(),
            "magpie".into(),
            "options".into(),
            "customHeader".into(),
        ],
        Some(&serde_json::json!("external")),
    )
    .expect("external field");
    fs::write(&f.path, changed).expect("edit");
    disconnect(&def, &f.path).expect("disconnect");
    let parsed = jsonc::parse(&f.bytes(), def.format).expect("config").value;
    assert_eq!(
        parsed["provider"]["magpie"]["options"]["customHeader"],
        "external"
    );
    assert!(
        parsed["provider"]["magpie"]["options"]
            .get("apiKey")
            .is_none()
    );
}
#[test]
fn jsonc_precise_edits_keep_trivia_unknown_fields_and_quoted_punctuation() {
    let def = definitions()
        .into_iter()
        .find(|d| d.format == ConfigFormat::Jsonc)
        .expect("jsonc fixture");
    let original=b"// file comment\n{\n  // model comment\n  \"model\" : \"old/model\", /* same-line note */\n  \"unknown\": {\"url\":\"http://example.invalid/a,}\",\"array\":[1,2,],},\n}\n";
    let f = Fixture::new(original);
    connect(&def, &f, "fixture/model-a");
    let connected = String::from_utf8(f.bytes()).expect("utf8");
    assert!(connected.contains("// model comment"));
    assert!(connected.contains("/* same-line note */"));
    assert!(
        connected
            .contains("\"unknown\": {\"url\":\"http://example.invalid/a,}\",\"array\":[1,2,],}")
    );
    commit_model_update(prepare_model_update(&def, &f.path, "fixture/model-b").expect("preview"))
        .expect("update");
    disconnect(&def, &f.path).expect("disconnect");
    assert_eq!(f.bytes(), original);
}
#[test]
fn invalid_missing_unsafe_and_duplicate_files_are_not_silently_replaced() {
    for def in definitions() {
        for bytes in [
            b"{ invalid fixture-secret".as_slice(),
            b"[]",
            b"{\"model\":1,\"model\":2}",
        ] {
            let f = Fixture::new(bytes);
            assert!(prepare_connect(&def, &f.path, "fixture/model-a", gateway()).is_err());
            assert_eq!(f.bytes(), bytes);
        }
        let f = Fixture::new(b"{}");
        fs::remove_file(&f.path).expect("remove fixture config");
        assert!(matches!(
            prepare_connect(&def, &f.path, "fixture/model-a", gateway()),
            Err(Error::MissingConfig)
        ));
        let actual = f.root.join("actual");
        fs::write(&actual, b"{}").expect("fixture");
        symlink(&actual, &f.path).expect("symlink");
        assert!(matches!(
            prepare_connect(&def, &f.path, "fixture/model-a", gateway()),
            Err(Error::Symlink)
        ));
        assert!(matches!(
            prepare_connect(
                &def,
                Path::new("relative.json"),
                "fixture/model-a",
                gateway()
            ),
            Err(Error::UnsafePath)
        ));
    }
}
#[test]
fn private_recovery_permissions_are_enforced_and_keys_never_enter_errors() {
    let def = definitions().remove(0);
    let f = Fixture::new(br#"{"env":{"ANTHROPIC_AUTH_TOKEN":"fixture-private-sentinel"}}"#);
    connect(&def, &f, "fixture/model-a");
    f.private();
    let journal = f.root.join(".config.jsonc.magpie-agent-state/journal.json");
    fs::set_permissions(&journal, fs::Permissions::from_mode(0o644))
        .expect("expose synthetic fixture");
    let e = read_selection(&def, &f.path).expect_err("reject exposed recovery");
    assert_eq!(e, Error::StateInvalid);
    assert!(!format!("{e:?} {e}").contains("fixture-private-sentinel"));
}
#[test]
fn failure_before_config_install_is_recoverable_without_touching_original() {
    for def in definitions() {
        let original = b"{\"model\":\"old/model\"}";
        let f = Fixture::new(original);
        let p = prepare_connect(&def, &f.path, "fixture/model-a", gateway()).expect("preview");
        test_support::arm(Point::BeforeInstall, &f.path, 1, Action::Fail);
        assert_eq!(commit_connect(p), Err(Error::RecoveryRequired));
        assert_eq!(f.bytes(), original);
        assert_eq!(
            read_selection(&def, &f.path).expect("selection").status,
            ConnectionStatus::RecoveryRequired
        );
        disconnect(&def, &f.path).expect("recover aborted connect");
        assert_eq!(f.bytes(), original);
    }
}
#[test]
fn failed_update_preserves_first_original_and_ambiguous_recovery_refuses_overwrite() {
    let def = definitions().remove(0);
    let f = Fixture::new(b"{\"model\":\"old/model\"}");
    connect(&def, &f, "fixture/model-a");
    let p = prepare_model_update(&def, &f.path, "fixture/model-b").expect("preview");
    test_support::arm(Point::BeforeInstall, &f.path, 1, Action::Fail);
    assert_eq!(commit_model_update(p), Err(Error::RecoveryRequired));
    let external = b"{\"model\":\"external/model\"}";
    fs::write(&f.path, external).expect("external edit");
    assert_eq!(disconnect(&def, &f.path), Err(Error::RecoveryRequired));
    assert_eq!(f.bytes(), external);
}
#[test]
fn generic_definition_has_no_vendor_dependency_and_validates_templates() {
    let def=AgentDefinition::from_manifest_json(r#"{"schema_version":1,"id":"synthetic","name":"Synthetic","config_paths":["${home}/synthetic.json"],"format":"json","selection":{"path":["chosen"],"strip_prefix":"local/"},"fields":[{"path":["chosen"],"value":"local/${model}"},{"path":["endpoint"],"value":"${gateway}/api"}]}"#).expect("generic definition");
    let f = Fixture::new(b"{\"chosen\":\"old/model\",\"unknown\":true}");
    connect(&def, &f, "fixture/model-a");
    assert_eq!(
        read_selection(&def, &f.path)
            .expect("selection")
            .model
            .as_deref(),
        Some("fixture/model-a")
    );
    disconnect(&def, &f.path).expect("restore");
    let mut invalid = def.clone();
    invalid.fields[0].value = serde_json::json!("${unknown}");
    assert_eq!(invalid.validate(), Err(Error::InvalidDefinition));
    invalid = def;
    invalid.fields.push(invalid.fields[0].clone());
    assert_eq!(invalid.validate(), Err(Error::InvalidDefinition));
}

// A fresh process is required to verify kernel lock release without Drop.
#[test]
#[ignore = "invoked only by the isolated SIGKILL parent test"]
fn crash_worker() {
    let path =
        PathBuf::from(std::env::var_os("MAGPIE_SYNTHETIC_CRASH_PATH").expect("fixture path"));
    let index: usize = std::env::var("MAGPIE_SYNTHETIC_DEFINITION")
        .expect("index")
        .parse()
        .expect("integer");
    let def = if index == 2 {
        super::toml_tests::definition()
    } else {
        definitions().remove(index)
    };
    let operation = std::env::var("MAGPIE_SYNTHETIC_OPERATION").expect("operation");
    let after = std::env::var("MAGPIE_SYNTHETIC_POINT").expect("point") == "after";
    let state = path
        .parent()
        .expect("parent")
        .join(".config.jsonc.magpie-agent-state");
    if after {
        test_support::arm(
            Point::BeforeDirectorySync,
            path.parent().expect("parent"),
            if state.exists() { 1 } else { 2 },
            Action::Pause,
        );
    } else {
        test_support::arm(Point::BeforeInstall, &path, 1, Action::Pause);
    }
    match operation.as_str() {
        "connect" => {
            commit_connect(
                prepare_connect(&def, &path, "fixture/model-a", gateway()).expect("preview"),
            )
            .expect("connect");
        }
        "update" => {
            commit_model_update(
                prepare_model_update(&def, &path, "fixture/model-b").expect("preview"),
            )
            .expect("update");
        }
        "disconnect" => {
            disconnect(&def, &path).expect("disconnect");
        }
        _ => panic!("unknown synthetic worker operation"),
    }
    panic!("synthetic checkpoint was not reached");
}
#[test]
fn sigkill_at_install_boundaries_recovers_and_kernel_lock_excludes_other_writers() {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};
    use std::time::Duration;
    for (index, def) in definitions()
        .into_iter()
        .chain(std::iter::once(super::toml_tests::definition()))
        .enumerate()
    {
        for operation in ["connect", "update", "disconnect"] {
            for point in ["before", "after"] {
                let original: &[u8] = if def.format == ConfigFormat::Toml {
                    b"# preserved\nmodel = \"old/model\"\nunknown = \"fixture-only\"\n"
                } else {
                    b"{\"model\":\"old/model\",\"unknown\":\"fixture-only\"}"
                };
                let f = Fixture::new(original);
                if operation != "connect" {
                    connect(&def, &f, "fixture/model-a");
                }
                let mut child = {
                    let _gate = test_support::spawn_scope();
                    Command::new(std::env::current_exe().expect("test executable"))
                        .args([
                            "--exact",
                            "agent_connection::tests::crash_worker",
                            "--ignored",
                            "--nocapture",
                        ])
                        .env("MAGPIE_SYNTHETIC_CRASH_PATH", &f.path)
                        .env("MAGPIE_SYNTHETIC_DEFINITION", index.to_string())
                        .env("MAGPIE_SYNTHETIC_OPERATION", operation)
                        .env("MAGPIE_SYNTHETIC_POINT", point)
                        .stdout(Stdio::piped())
                        .stderr(Stdio::piped())
                        .spawn()
                        .expect("spawn synthetic worker")
                };
                let stdout = child.stdout.take().expect("worker stdout");
                let (tx, rx) = std::sync::mpsc::channel();
                let reader = std::thread::spawn(move || {
                    for line in BufReader::new(stdout).lines() {
                        if line
                            .expect("worker output")
                            .contains("magpie-transaction-checkpoint")
                        {
                            let _ = tx.send(());
                            return;
                        }
                    }
                });
                let ready = rx.recv_timeout(Duration::from_secs(10));
                if ready.is_ok() {
                    assert_eq!(
                        read_selection(&def, &f.path),
                        Err(Error::LockBusy),
                        "{operation}/{point} excludes concurrent reads"
                    );
                    assert!(matches!(
                        prepare_connect(&def, &f.path, "fixture/model-c", gateway()),
                        Err(Error::LockBusy)
                    ));
                }
                let _ = child.kill();
                let result = child.wait().expect("reap synthetic worker");
                reader.join().expect("join output reader");
                assert!(
                    ready.is_ok(),
                    "worker did not reach {operation}/{point}; exit {result}"
                );
                assert_eq!(
                    read_selection(&def, &f.path)
                        .expect("pending selection")
                        .status,
                    ConnectionStatus::RecoveryRequired
                );
                disconnect(&def, &f.path).expect("recover killed transaction and disconnect");
                assert_eq!(
                    f.bytes(),
                    original,
                    "{operation}/{point} restores first original"
                );
                f.private();
            }
        }
    }
}
#[test]
fn unsupported_owned_shapes_and_non_loopback_gateways_fail_without_writes() {
    let def = definitions().remove(0);
    let f = Fixture::new(br#"{"model":{"unknown":"keep"}}"#);
    assert!(matches!(
        prepare_connect(&def, &f.path, "fixture/model-a", gateway()),
        Err(Error::UnsupportedShape)
    ));
    let f = Fixture::new(b"{}");
    assert!(matches!(
        prepare_connect(
            &def,
            &f.path,
            "fixture/model-a",
            "192.0.2.1:1234".parse().expect("literal")
        ),
        Err(Error::InvalidGateway)
    ));
    assert!(matches!(
        prepare_connect(&def, &f.path, "bad model", gateway()),
        Err(Error::InvalidModel)
    ));
    assert_eq!(fs::read_dir(&f.root).expect("directory").count(), 1);
}

#[test]
fn corrupted_recovery_content_is_refused_without_overwriting_config() {
    let def = definitions().remove(0);
    let f = Fixture::new(br#"{"model":"original/model"}"#);
    connect(&def, &f, "fixture/model-a");
    let journal = f.root.join(".config.jsonc.magpie-agent-state/journal.json");
    let text = fs::read_to_string(&journal).expect("private synthetic recovery");
    let altered = text.replace("original/model", "tampered/model");
    assert_ne!(altered, text);
    fs::write(&journal, altered).expect("corrupt fixture recovery");
    let before = f.bytes();
    assert_eq!(disconnect(&def, &f.path), Err(Error::StateInvalid));
    assert_eq!(f.bytes(), before);
}
#[test]
fn parameterized_key_templates_restore_and_definition_changes_require_reconciliation() {
    let mut def = definitions().remove(0);
    def.id = "synthetic-template".into();
    def.fields.push(OwnedFieldSpec {
        path: vec!["models".into(), "local-${model}".into()],
        value: serde_json::json!("${model}"),
    });
    let f = Fixture::new(b"{}");
    connect(&def, &f, "fixture/model-a");
    assert_eq!(
        read_selection(&def, &f.path).expect("selection").status,
        ConnectionStatus::Connected
    );
    let mut changed = def.clone();
    changed.name.push_str(" changed definition");
    assert_eq!(read_selection(&changed, &f.path), Err(Error::StateInvalid));
    disconnect(&def, &f.path).expect("restore with exact definition");
    assert_eq!(f.bytes(), b"{}");
}

#[test]
fn jsonc_preserves_crlf_tabs_and_comments_inside_removed_owned_containers() {
    let def = definitions()
        .into_iter()
        .find(|d| d.format == ConfigFormat::Jsonc)
        .expect("jsonc fixture");
    let original = b"{\r\n\t\"model\": \"old/model\",\r\n\t\"unknown\": true\r\n}\r\n";
    let f = Fixture::new(original);
    connect(&def, &f, "fixture/model-a");
    let text = String::from_utf8(f.bytes()).expect("utf8");
    assert!(text.contains("\r\n\t\"small_model\""));
    assert!(text.contains("\r\n\t\"provider\""));
    let changed = text.replace("\"npm\":", "/* external provider note */ \"npm\":");
    fs::write(&f.path, changed).expect("external note");
    disconnect(&def, &f.path).expect("disconnect");
    let after = String::from_utf8(f.bytes()).expect("utf8");
    assert!(after.contains("/* external provider note */"));
    let value = jsonc::parse(after.as_bytes(), def.format)
        .expect("restored jsonc")
        .value;
    assert_eq!(value["model"], "old/model");
    assert_eq!(value["unknown"], true);
    assert!(value.get("provider").is_none());
}

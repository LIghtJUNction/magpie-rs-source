use super::tests::{Fixture, connect, gateway};
use super::*;
use std::fs;

pub(super) fn definition() -> AgentDefinition {
    AgentDefinition::from_manifest_json(
        r#"{
        "schema_version":1,"id":"synthetic-toml","name":"Synthetic TOML",
        "config_paths":[],"format":"toml","selection":{"path":["model"]},
        "fields":[{"path":["model"],"value":"${model}"},
          {"path":["routes","local","endpoint"],"value":"${gateway}/v1"},
          {"path":["routes","local","enabled"],"value":true},
          {"path":["routes","local","budget"],"value":8192}]
    }"#,
    )
    .expect("vendor-neutral definition")
}

#[test]
fn toml_model_changes_preserve_unknown_syntax_and_disconnect_restores_exact_bytes() {
    let original = b"# untouched\r\nmodel = 'old/model' # selection comment\r\ndate = 1979-05-27T07:32:00Z\r\nnan = nan\r\narr = [1, 'two', { k = true }]\r\n[routes.other]\r\nendpoint = 'fixture-only' # leave this\r\n";
    let f = Fixture::new(original);
    let def = definition();
    let preview = prepare_connect(&def, &f.path, "fixture/model-a", gateway()).unwrap();
    assert_eq!(f.bytes(), original);
    assert_eq!(fs::read_dir(&f.root).unwrap().count(), 1);
    commit_connect(preview).unwrap();
    let connected = String::from_utf8(f.bytes()).unwrap();
    for text in [
        "# untouched",
        "# selection comment",
        "date = 1979-05-27T07:32:00Z",
        "nan = nan",
        "arr = [1, 'two', { k = true }]",
        "endpoint = 'fixture-only' # leave this",
    ] {
        assert!(connected.contains(text), "preserves {text}");
    }
    commit_model_update(prepare_model_update(&def, &f.path, "fixture/model-b").unwrap()).unwrap();
    assert_eq!(
        read_selection(&def, &f.path).unwrap().model.as_deref(),
        Some("fixture/model-b")
    );
    disconnect(&def, &f.path).unwrap();
    assert_eq!(f.bytes(), original);
}

#[test]
fn toml_stale_preview_and_owned_drift_are_refused_but_external_fields_survive_disconnect() {
    let original = b"# private\nmodel = 'old/model' # keep selection comment\nextra = 1\n";
    let f = Fixture::new(original);
    let def = definition();
    let stale = prepare_connect(&def, &f.path, "fixture/a", gateway()).unwrap();
    fs::write(&f.path, [original.as_slice(), b"other = true\n"].concat()).unwrap();
    assert_eq!(commit_connect(stale), Err(Error::OriginalChanged));
    connect(&def, &f, "fixture/a");
    let stale = prepare_model_update(&def, &f.path, "fixture/b").unwrap();
    commit_model_update(prepare_model_update(&def, &f.path, "fixture/c").unwrap()).unwrap();
    assert_eq!(commit_model_update(stale), Err(Error::OriginalChanged));
    let edited = String::from_utf8(f.bytes())
        .unwrap()
        .replace("extra = 1", "extra = 9 # outside change")
        .replace("fixture/c", "manual/drift");
    fs::write(&f.path, edited).unwrap();
    assert_eq!(
        read_selection(&def, &f.path).unwrap().status,
        ConnectionStatus::Drifted
    );
    assert!(matches!(
        prepare_model_update(&def, &f.path, "fixture/d"),
        Err(Error::OriginalChanged)
    ));
    let restored = disconnect(&def, &f.path).unwrap();
    assert_eq!(restored.preserved_fields, 1);
    let after = String::from_utf8(f.bytes()).unwrap();
    assert!(after.contains("manual/drift"));
    assert!(after.contains("extra = 9 # outside change"));
    assert!(after.contains("# keep selection comment"));
    assert!(
        document::parse(after.as_bytes(), ConfigFormat::Toml)
            .unwrap()
            .value
            .get("routes")
            .is_none()
    );
}

#[test]
fn toml_literal_keys_inline_tables_and_restricted_scalar_types_fail_closed() {
    let original =
        b"model = 'old'\nroutes = { other = { endpoint = 'unchanged' } } # preserve inline\n";
    let f = Fixture::new(original);
    let def = definition();
    connect(&def, &f, "fixture/m.1");
    assert!(
        String::from_utf8(f.bytes())
            .unwrap()
            .contains("# preserve inline")
    );
    disconnect(&def, &f.path).unwrap();
    assert_eq!(f.bytes(), original);
    let bytes = document::set(
        b"",
        ConfigFormat::Toml,
        &["literal.dot/slash".into(), "field".into()],
        Some(&serde_json::json!("value")),
    )
    .unwrap();
    let parsed = document::parse(&bytes, ConfigFormat::Toml).unwrap().value;
    assert_eq!(parsed["literal.dot/slash"]["field"], "value");
    for bytes in [
        b"model = 1979-05-27\n".as_slice(),
        b"model = nan\n",
        b"model = []\n",
        b"model='a'\nmodel='b'\n",
        b"model = \"secret-sentinel\"\n[broken",
    ] {
        let f = Fixture::new(bytes);
        assert!(prepare_connect(&def, &f.path, "fixture/a", gateway()).is_err());
        assert_eq!(f.bytes(), bytes);
        assert_eq!(fs::read_dir(&f.root).unwrap().count(), 1);
    }
    for unsupported in [Value::Null, serde_json::json!(u64::MAX)] {
        let mut bad = def.clone();
        bad.fields[1].value = unsupported;
        assert_eq!(bad.validate(), Err(Error::InvalidDefinition));
    }
}

#[test]
fn toml_removing_new_owned_fields_preserves_comments_added_after_connect() {
    let f = Fixture::new(b"model = 'old'\n");
    let def = definition();
    connect(&def, &f, "fixture/a");
    // Turn the introduced route into a regular table to exercise key/value decoration.
    let doc = b"model = 'fixture/a'\n# external note on container\n[routes.local]\n# external note on key\nendpoint = 'http://127.0.0.1:41327/v1' # external suffix\nenabled = true\nbudget = 8192\n";
    fs::write(&f.path, doc).unwrap();
    assert_eq!(
        read_selection(&def, &f.path).unwrap().status,
        ConnectionStatus::Connected
    );
    disconnect(&def, &f.path).unwrap();
    let after = String::from_utf8(f.bytes()).unwrap();
    for comment in [
        "# external note on container",
        "# external note on key",
        "# external suffix",
    ] {
        assert!(after.contains(comment), "keeps {comment}: {after}");
    }
    assert_eq!(
        document::parse(after.as_bytes(), ConfigFormat::Toml)
            .unwrap()
            .value,
        serde_json::json!({"model":"old"})
    );
}

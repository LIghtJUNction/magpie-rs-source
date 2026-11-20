//! Current plugin/CLI selection contract; all configuration and recovery state is synthetic.
#![cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
use serde_json::{Value, json};
use std::{
    fs::{self, DirBuilder, OpenOptions},
    io::Write,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "magpie-codex-selection-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        DirBuilder::new().mode(0o700).create(&root).unwrap();
        Self(root)
    }
    fn path(&self) -> PathBuf {
        self.0.join("config.toml")
    }
    fn write(&self, source: &str) {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(self.path())
            .unwrap();
        file.write_all(source.as_bytes()).unwrap();
    }
    fn call(&self, operation: &str, options: &[&str]) -> Value {
        let output = Command::new(env!("CARGO_BIN_EXE_magpie"))
            .env_clear()
            .env("MAGPIE_CONFIG_DIR", self.0.join("private"))
            .args(["plugins", "call", "agent.codex", operation, "--config"])
            .arg(self.path())
            .args(options)
            .arg("--json")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let output: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(output["plugin_id"], "agent.codex");
        assert_eq!(output["operation"], operation);
        assert!(output["version"].is_string());
        output["result"].clone()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn field<'a>(selection: &'a Value, id: &str) -> &'a Value {
    let fields = selection["public_fields"].as_array().unwrap();
    assert_eq!(fields.len(), 2);
    for field in fields {
        let labels = field["label"].as_object().unwrap();
        assert_eq!(labels.len(), 5);
        for locale in ["en", "zh-CN", "zh-TW", "ja", "de"] {
            let label = labels[locale].as_str().unwrap();
            assert!(!label.is_empty() && label.len() <= 128);
            assert!(!label.chars().any(char::is_control));
        }
    }
    &fields.iter().find(|field| field["id"] == id).unwrap()["value"]
}
fn assert_selection(fixture: &Fixture, model: &str, state: &str, provider: &str, effort: &str) {
    let output = fixture.call("selection", &[]);
    assert_eq!(output["selection"], json!({"model":model,"state":state}));
    assert_eq!(output["selection_scope"], "file");
    assert_eq!(field(&output, "model_provider"), provider);
    assert_eq!(field(&output, "model_reasoning_effort"), effort);
    assert!(!output.to_string().contains("PRIVATE_CANARY"));
}

#[test]
fn original_public_fields_survive_uniform_connect_model_and_exact_disconnect() {
    let fixture = Fixture::new();
    let original = "# Preserve exact original bytes.\nmodel = 'gpt-5'\nmodel_provider = 'original-provider'\nmodel_reasoning_effort = 'medium'\n\n[model_providers.original-provider]\nname='PRIVATE_CANARY_DEFINITION'\nbase_url='https://fixture.invalid/v1'\n[model_providers.original-provider.http_headers]\nAuthorization='PRIVATE_CANARY_HEADER'\n";
    fixture.write(original);
    assert_selection(&fixture, "gpt-5", "external", "original-provider", "medium");
    let preview = fixture.call(
        "preview",
        &["--model", "fixture/first", "--listen", "127.0.0.1:43197"],
    );
    assert_eq!(fs::read(fixture.path()).unwrap(), original.as_bytes());
    assert_selection(&fixture, "gpt-5", "external", "original-provider", "medium");
    let revision = preview["revision"].as_str().unwrap();
    assert_eq!(revision.len(), 64);
    fixture.call(
        "connect",
        &[
            "--model",
            "fixture/first",
            "--listen",
            "127.0.0.1:43197",
            "--if-revision",
            revision,
        ],
    );
    assert_selection(&fixture, "fixture/first", "qualified", "magpie", "medium");
    let connected = fs::read(fixture.path()).unwrap();
    let preview = fixture.call("model-preview", &["--model", "fixture/second"]);
    assert_eq!(fs::read(fixture.path()).unwrap(), connected);
    fixture.call(
        "model",
        &[
            "--model",
            "fixture/second",
            "--if-revision",
            preview["revision"].as_str().unwrap(),
        ],
    );
    assert_selection(&fixture, "fixture/second", "qualified", "magpie", "medium");
    fixture.call("disconnect", &[]);
    assert_eq!(fs::read(fixture.path()).unwrap(), original.as_bytes());
    assert_selection(&fixture, "gpt-5", "external", "original-provider", "medium");
}

#[test]
fn selection_never_infers_missing_empty_or_nonstring_public_values() {
    let fixture = Fixture::new();
    let missing = fixture.call("selection", &[]);
    assert_eq!(missing["selection"], json!({"model":null,"state":"unset"}));
    assert!(field(&missing, "model_provider").is_null());
    assert!(field(&missing, "model_reasoning_effort").is_null());
    assert!(!fixture.path().exists());
    for (source, state) in [
        (
            "[model_providers.example]\nmodel='not-top-level'\nmodel_provider='not-top-level'\n",
            "unset",
        ),
        (
            "model=42\nmodel_provider=[]\nmodel_reasoning_effort=false\n",
            "unset",
        ),
        (
            "model=''\nmodel_provider=''\nmodel_reasoning_effort=''\n",
            "unreadable",
        ),
    ] {
        fixture.write(source);
        let output = fixture.call("selection", &[]);
        assert_eq!(output["selection"], json!({"model":null,"state":state}));
        assert!(field(&output, "model_provider").is_null());
        assert!(field(&output, "model_reasoning_effort").is_null());
        assert_eq!(fs::read_to_string(fixture.path()).unwrap(), source);
    }
}

#[test]
fn public_strings_are_bounded_and_control_characters_are_not_returned() {
    let fixture = Fixture::new();
    let bound = "x".repeat(512);
    fixture.write(&format!(
        "model='{bound}'\nmodel_provider='{bound}'\nmodel_reasoning_effort='high'\n"
    ));
    let output = fixture.call("selection", &[]);
    assert_eq!(
        output["selection"],
        json!({"model":bound,"state":"external"})
    );
    assert_eq!(field(&output, "model_provider"), &bound);
    let oversized = "x".repeat(513);
    for source in [
        format!(
            "model='{oversized}'\nmodel_provider='{oversized}'\nmodel_reasoning_effort='{oversized}'\n"
        ),
        "model=\"a\\nb\"\nmodel_provider=\"a\\tb\"\nmodel_reasoning_effort=\"a\\u007fb\"\n".into(),
    ] {
        fixture.write(&source);
        let output = fixture.call("selection", &[]);
        assert_eq!(
            output["selection"],
            json!({"model":null,"state":"unreadable"})
        );
        assert!(field(&output, "model_provider").is_null());
        assert!(field(&output, "model_reasoning_effort").is_null());
        assert_eq!(fs::read_to_string(fixture.path()).unwrap(), source);
    }
}

//! Codex-specific executable, configuration-path and selection discovery.
use crate::codex_config::{self, CodexConfigError, CodexSelection};
use magpie_core::config_probe::{ConfigFileState, find_executable};
use std::{env, path::PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexDiscovery {
    pub executable: Option<PathBuf>,
    pub config_path: Option<PathBuf>,
    pub config_state: ConfigFileState,
    pub config_error_line: Option<usize>,
    /// Explicit selections from the same config read that determined its state.
    ///
    /// Empty when the file is unavailable, missing, or invalid. No credentials
    /// or provider definitions are included.
    pub selection: CodexSelection,
}

pub fn discover_codex() -> CodexDiscovery {
    let executable = find_codex_executable();
    let config_path = codex_config_path();
    let (config_state, config_error_line, selection) =
        config_path.as_deref().map(inspect_config_file).unwrap_or((
            ConfigFileState::Unavailable,
            None,
            CodexSelection::default(),
        ));

    CodexDiscovery {
        executable,
        config_path,
        config_state,
        config_error_line,
        selection,
    }
}

pub fn find_codex_executable() -> Option<PathBuf> {
    find_executable("codex")
}

pub fn codex_config_path() -> Option<PathBuf> {
    let home = env::var_os("CODEX_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| magpie_core::config_probe::user_home_dir().map(|home| home.join(".codex")))?;

    let home = if home.is_absolute() {
        home
    } else {
        env::current_dir().ok()?.join(home)
    };

    Some(home.join("config.toml"))
}

fn inspect_config_file(path: &std::path::Path) -> (ConfigFileState, Option<usize>, CodexSelection) {
    let (state, line) = match codex_config::read_selection(path) {
        Ok(Some(selection)) => return (ConfigFileState::ValidToml, None, selection),
        Ok(None) => (ConfigFileState::Missing, None),
        Err(CodexConfigError::InvalidToml { line }) => (ConfigFileState::InvalidToml, line),
        Err(CodexConfigError::TooLarge) => (ConfigFileState::TooLarge, None),
        Err(CodexConfigError::InvalidUtf8) => (ConfigFileState::InvalidUtf8, None),
        Err(CodexConfigError::NotAFile) => (ConfigFileState::NotAFile, None),
        Err(CodexConfigError::Symlink) => (ConfigFileState::Symlink, None),
        Err(_) => (ConfigFileState::Unreadable, None),
    };
    (state, line, CodexSelection::default())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let sequence = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("test clock is after the Unix epoch")
                .as_nanos();
            let root = std::env::temp_dir().join(format!(
                "magpie-discovery-{}-{timestamp}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&root).expect("create isolated discovery directory");
            Self(root)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn configured_selection_and_discovery_state_come_from_one_read_path() {
        let fixture = Fixture::new();
        let config = fixture.0.join("config.toml");
        fs::write(
            &config,
            b"model='selected'\nmodel_provider='native'\nmodel_reasoning_effort='medium'\napi_key='fixture-secret'",
        )
        .expect("write Codex fixture");
        let (state, line, selection) = inspect_config_file(&config);
        assert_eq!(state, ConfigFileState::ValidToml);
        assert_eq!(line, None);
        assert_eq!(selection.model.as_deref(), Some("selected"));
        assert_eq!(selection.model_provider.as_deref(), Some("native"));
        assert_eq!(selection.model_reasoning_effort.as_deref(), Some("medium"));
        assert!(!format!("{selection:?}").contains("fixture-secret"));
    }

    #[test]
    fn invalid_or_missing_config_does_not_produce_fake_selections() {
        let fixture = Fixture::new();
        let config = fixture.0.join("config.toml");
        assert_eq!(
            inspect_config_file(&config),
            (ConfigFileState::Missing, None, CodexSelection::default())
        );
        fs::write(
            &config,
            b"model='would-be-selected'\napi_key=\"fixture-secret-unclosed\n",
        )
        .expect("write malformed Codex fixture");
        assert_eq!(
            inspect_config_file(&config),
            (
                ConfigFileState::InvalidToml,
                Some(2),
                CodexSelection::default()
            )
        );
        assert_eq!(
            inspect_config_file(&fixture.0),
            (ConfigFileState::NotAFile, None, CodexSelection::default())
        );
    }
}

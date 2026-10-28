use std::path::PathBuf;

use magpie_core::agents::{self, ConfigFileState};

#[derive(Clone, Debug)]
pub(crate) struct ModelSettings {
    pub model: Option<String>,
    pub provider: Option<String>,
    pub effort: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct Config {
    pub path: Option<PathBuf>,
    pub state: ConfigFileState,
    pub error_line: Option<usize>,
    pub fields_readable: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct Agent {
    pub id: &'static str,
    pub name: &'static str,
    pub icon: Option<&'static str>,
    pub executable: PathBuf,
    pub config: Option<Config>,
    pub settings: Option<ModelSettings>,
    pub last_known: bool,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Snapshot {
    pub agents: Vec<Agent>,
}

impl Snapshot {
    pub fn read() -> Self {
        let codex = agents::discover_codex();
        let agents = agents::discover_installed_agents()
            .into_iter()
            .map(|installed| {
                let is_codex = installed.id == "codex";
                let readable = is_codex && codex.config_state == ConfigFileState::ValidToml;
                Agent {
                    id: installed.id,
                    name: installed.name,
                    icon: match installed.id {
                        "codex" => Some("icons/codex-color.svg"),
                        "claude" => Some("icons/claudecode-color.svg"),
                        "gemini" => Some("icons/geminicli-color.svg"),
                        "opencode" => Some("icons/opencode.svg"),
                        _ => None,
                    },
                    executable: installed.executable,
                    config: is_codex.then(|| Config {
                        path: codex.config_path.clone(),
                        state: codex.config_state,
                        error_line: codex.config_error_line,
                        fields_readable: readable,
                    }),
                    settings: readable.then(|| ModelSettings {
                        model: codex.selection.model.clone(),
                        provider: codex.selection.model_provider.clone(),
                        effort: codex.selection.model_reasoning_effort.clone(),
                    }),
                    last_known: false,
                }
            })
            .collect();
        Self { agents }
    }

    pub fn retain_last_readable(&mut self, previous: &Self) {
        for agent in &mut self.agents {
            if agent.settings.is_some()
                || agent
                    .config
                    .as_ref()
                    .is_none_or(|config| config.fields_readable)
            {
                continue;
            }
            if let Some(settings) = previous
                .agents
                .iter()
                .find(|old| old.id == agent.id)
                .and_then(|old| old.settings.clone())
            {
                agent.settings = Some(settings);
                agent.last_known = true;
            }
        }
    }
}

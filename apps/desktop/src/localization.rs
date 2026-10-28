use std::env;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Locale {
    English,
    SimplifiedChinese,
    TraditionalChinese,
    Japanese,
    German,
}

impl Locale {
    pub const ALL: [Self; 5] = [
        Self::English,
        Self::SimplifiedChinese,
        Self::TraditionalChinese,
        Self::Japanese,
        Self::German,
    ];

    pub fn system() -> Self {
        let locale = ["LC_ALL", "LC_MESSAGES", "LANG"]
            .into_iter()
            .filter_map(|name| env::var(name).ok())
            .find(|value| !value.is_empty())
            .unwrap_or_default()
            .replace('-', "_")
            .to_ascii_lowercase();
        if locale.starts_with("zh_tw")
            || locale.starts_with("zh_hk")
            || locale.starts_with("zh_hant")
        {
            Self::TraditionalChinese
        } else if locale.starts_with("zh") {
            Self::SimplifiedChinese
        } else if locale.starts_with("ja") {
            Self::Japanese
        } else if locale.starts_with("de") {
            Self::German
        } else {
            Self::English
        }
    }

    pub const fn id(self) -> &'static str {
        match self {
            Self::English => "locale-en",
            Self::SimplifiedChinese => "locale-zh-cn",
            Self::TraditionalChinese => "locale-zh-tw",
            Self::Japanese => "locale-ja",
            Self::German => "locale-de",
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::English => "English",
            Self::SimplifiedChinese => "简体中文",
            Self::TraditionalChinese => "繁體中文",
            Self::Japanese => "日本語",
            Self::German => "Deutsch",
        }
    }

    pub fn text(self, key: Text) -> &'static str {
        let values = match key {
            Text::ConfigPath => [
                "Configuration path",
                "配置路径",
                "設定路徑",
                "設定ファイルの場所",
                "Konfigurationspfad",
            ],
            Text::CopyPath => [
                "Copy path",
                "复制路径",
                "複製路徑",
                "パスをコピー",
                "Pfad kopieren",
            ],
            Text::Copied => [
                "Path copied",
                "路径已复制",
                "路徑已複製",
                "パスをコピーしました",
                "Pfad kopiert",
            ],
            Text::Executable => [
                "Executable",
                "程序路径",
                "程式路徑",
                "実行ファイル",
                "Programmdatei",
            ],
            Text::ValidHint => [
                "Valid TOML. Credentials and model availability are not checked.",
                "TOML 格式有效，尚未验证凭据或模型可用性。",
                "TOML 格式有效，尚未驗證憑證或模型可用性。",
                "有効な TOML です。認証情報やモデルの利用可否は未確認です。",
                "Gültiges TOML. Zugangsdaten und Modellverfügbarkeit sind nicht geprüft.",
            ],
            Text::MissingHint => [
                "No file at this path. Set up Codex, then refresh; Magpie will not create the file.",
                "此路径没有文件。先配置 Codex 后刷新；Magpie 不会创建文件。",
                "此路徑沒有檔案。先設定 Codex 後重新整理；Magpie 不會建立檔案。",
                "ファイルがありません。Codex を設定して更新してください。Magpie はファイルを作成しません。",
                "Keine Datei an diesem Pfad. Codex einrichten und aktualisieren; Magpie erstellt keine Datei.",
            ],
            Text::InvalidHint => [
                "Fix the TOML syntax in the file and refresh. File contents are not displayed.",
                "修正文件中的 TOML 语法后刷新。此处不显示文件内容。",
                "修正檔案中的 TOML 語法後重新整理。此處不顯示檔案內容。",
                "TOML 構文を修正して更新してください。ファイルの内容は表示しません。",
                "TOML-Syntax korrigieren und aktualisieren. Dateiinhalte werden nicht angezeigt.",
            ],
            Text::TooLargeHint => [
                "Check that this path points to the intended Codex config file.",
                "检查路径是否指向正确的 Codex 配置文件。",
                "檢查路徑是否指向正確的 Codex 設定檔。",
                "Codex の設定先が正しいか確認してください。",
                "Prüfen, ob der Pfad auf die richtige Codex-Konfiguration zeigt.",
            ],
            Text::Utf8Hint => [
                "Save the intended config as UTF-8, then refresh.",
                "将正确的配置保存为 UTF-8 后刷新。",
                "將正確的設定儲存為 UTF-8 後重新整理。",
                "設定を UTF-8 で保存して更新してください。",
                "Die Konfiguration als UTF-8 speichern und aktualisieren.",
            ],
            Text::UnreadableHint => [
                "Check the path and your read permissions, then refresh.",
                "检查路径和读取权限后刷新。",
                "檢查路徑和讀取權限後重新整理。",
                "パスと読み取り権限を確認して更新してください。",
                "Pfad und Leseberechtigungen prüfen und aktualisieren.",
            ],
            Text::NotAFileHint => [
                "Set CODEX_HOME to the directory containing config.toml and refresh.",
                "将 CODEX_HOME 指向包含 config.toml 的目录后刷新。",
                "將 CODEX_HOME 指向包含 config.toml 的目錄後重新整理。",
                "CODEX_HOME を config.toml のあるディレクトリに設定して更新してください。",
                "CODEX_HOME auf das Verzeichnis mit config.toml setzen und aktualisieren.",
            ],
            Text::SymlinkHint => [
                "This early version does not follow config symlinks. Review the target and use a regular file.",
                "此早期版本不跟随配置符号链接。请检查目标并使用常规文件。",
                "此早期版本不跟隨設定符號連結。請檢查目標並使用一般檔案。",
                "この初期版はリンクをたどりません。参照先を確認し通常のファイルを使用してください。",
                "Diese frühe Version folgt keinen Konfigurationslinks. Ziel prüfen und eine reguläre Datei verwenden.",
            ],
            Text::UnavailableHint => [
                "Set CODEX_HOME or your home directory and restart Magpie.",
                "设置 CODEX_HOME 或用户主目录后重启 Magpie。",
                "設定 CODEX_HOME 或使用者家目錄後重新啟動 Magpie。",
                "CODEX_HOME またはホームディレクトリを設定して再起動してください。",
                "CODEX_HOME oder das Benutzerverzeichnis setzen und Magpie neu starten.",
            ],
            Text::ErrorLine => [
                "Check line",
                "检查行号",
                "檢查行號",
                "確認する行",
                "Zeile prüfen",
            ],
            Text::Agents => ["Agents", "Agent", "Agent", "エージェント", "Agents"],
            Text::Settings => ["Settings", "设置", "設定", "設定", "Einstellungen"],
            Text::Intro => [
                "Your installed coding agents, in one place.",
                "在这里查看本机编码 Agent。",
                "在這裡查看本機編碼 Agent。",
                "インストール済みのコーディングエージェント。",
                "Ihre installierten Coding-Agents an einem Ort.",
            ],
            Text::LocalAgents => [
                "On this computer",
                "本机 Agent",
                "本機 Agent",
                "このコンピューター",
                "Auf diesem Computer",
            ],
            Text::Refresh => ["Refresh", "刷新", "重新整理", "更新", "Aktualisieren"],
            Text::Refreshing => [
                "Scanning local agents…",
                "正在扫描本机 Agent…",
                "正在掃描本機 Agent…",
                "エージェントを確認中…",
                "Lokale Agents werden geprüft…",
            ],
            Text::Preview => ["Preview", "预览版", "預覽版", "プレビュー", "Vorschau"],
            Text::Installed => [
                "Installed",
                "已安装",
                "已安裝",
                "インストール済み",
                "Installiert",
            ],
            Text::Model => [
                "Current model",
                "当前模型",
                "目前模型",
                "現在のモデル",
                "Aktuelles Modell",
            ],
            Text::AgentDefault => [
                "Agent default",
                "Agent 默认值",
                "Agent 預設值",
                "エージェントの既定値",
                "Agent-Standard",
            ],
            Text::Effort => ["Reasoning", "推理强度", "推理強度", "推論", "Denkaufwand"],
            Text::Provider => ["Provider", "供应商", "供應商", "プロバイダー", "Anbieter"],
            Text::Details => ["Details", "详情", "詳細資訊", "詳細", "Details"],
            Text::HideDetails => [
                "Hide details",
                "收起详情",
                "收合詳細資訊",
                "詳細を閉じる",
                "Details schließen",
            ],
            Text::ConfigReady => [
                "Config readable",
                "配置可读取",
                "設定可讀取",
                "設定を読取可能",
                "Konfiguration lesbar",
            ],
            Text::ConfigMissing => [
                "No config file",
                "没有配置文件",
                "沒有設定檔",
                "設定ファイルなし",
                "Keine Konfigurationsdatei",
            ],
            Text::ConfigInvalid => [
                "Invalid TOML",
                "TOML 格式有误",
                "TOML 格式有誤",
                "TOML が不正",
                "Ungültiges TOML",
            ],
            Text::ConfigLarge => [
                "Config too large",
                "配置文件过大",
                "設定檔過大",
                "設定が大きすぎます",
                "Konfiguration zu groß",
            ],
            Text::ConfigEncoding => [
                "Invalid text encoding",
                "文本编码错误",
                "文字編碼錯誤",
                "文字コードが不正",
                "Ungültige Textkodierung",
            ],
            Text::ConfigUnreadable => [
                "Config unreadable",
                "配置不可读取",
                "設定不可讀取",
                "設定を読取不可",
                "Konfiguration nicht lesbar",
            ],
            Text::ConfigNotFile => [
                "Config is not a file",
                "配置路径不是文件",
                "設定路徑不是檔案",
                "設定がファイルではありません",
                "Konfiguration ist keine Datei",
            ],
            Text::ConfigSymlink => [
                "Config is a symlink",
                "配置是符号链接",
                "設定是符號連結",
                "設定がシンボリックリンク",
                "Konfiguration ist ein Link",
            ],
            Text::ConfigUnavailable => [
                "Config location unknown",
                "配置位置未知",
                "設定位置未知",
                "設定の場所が不明",
                "Konfigurationsort unbekannt",
            ],
            Text::ConfigUnsupported => [
                "Unsupported config value",
                "配置字段格式不受支持",
                "設定欄位格式不支援",
                "未対応の設定値",
                "Nicht unterstützter Konfigurationswert",
            ],
            Text::UnsupportedHint => [
                "Model settings could not be read. Check their value types, then refresh.",
                "无法读取模型设置。请检查字段类型后刷新。",
                "無法讀取模型設定。請檢查欄位類型後重新整理。",
                "モデル設定を読み取れません。値の型を確認して更新してください。",
                "Modelleinstellungen konnten nicht gelesen werden. Werttypen prüfen und aktualisieren.",
            ],
            Text::ConfigSource => [
                "From the local config file; session overrides and connectivity are not checked.",
                "来自本地配置文件；未检查会话覆盖或连接状态。",
                "來自本機設定檔；未檢查工作階段覆寫或連線狀態。",
                "ローカル設定の値です。セッションの上書きや接続は未確認です。",
                "Aus der lokalen Konfiguration; Sitzungswerte und Verbindung sind nicht geprüft.",
            ],
            Text::OtherAgentHint => [
                "This preview detects the executable. Read its model settings in the Agent itself.",
                "此预览版只检测程序，模型设置请在 Agent 内查看。",
                "此預覽版僅偵測程式，模型設定請在 Agent 內查看。",
                "このプレビューは実行ファイルを検出します。モデルはエージェントで確認してください。",
                "Diese Vorschau erkennt die Programmdatei. Modelleinstellungen im Agent selbst prüfen.",
            ],
            Text::EmptyTitle => [
                "No local agents found",
                "未发现本机 Agent",
                "未發現本機 Agent",
                "エージェントが見つかりません",
                "Keine lokalen Agents gefunden",
            ],
            Text::EmptyHint => [
                "Install a supported Agent or add it to PATH, then refresh.",
                "安装支持的 Agent 或将其加入 PATH 后刷新。",
                "安裝支援的 Agent 或將其加入 PATH 後重新整理。",
                "対応するエージェントをインストールするか PATH に追加して更新してください。",
                "Einen unterstützten Agent installieren oder zum PATH hinzufügen und aktualisieren.",
            ],
            Text::Language => ["Language", "语言", "語言", "言語", "Sprache"],
            Text::Appearance => ["Appearance", "外观", "外觀", "外観", "Darstellung"],
            Text::System => ["System", "跟随系统", "跟隨系統", "システム", "System"],
            Text::Light => ["Light", "浅色", "淺色", "ライト", "Hell"],
            Text::Dark => ["Dark", "深色", "深色", "ダーク", "Dunkel"],
            Text::Preferences => [
                "Make yourself at home.",
                "按你的习惯调整界面。",
                "依你的習慣調整介面。",
                "使いやすい表示に。",
                "Passen Sie die Ansicht an.",
            ],
            Text::About => [
                "About this preview",
                "关于此预览版",
                "關於此預覽版",
                "このプレビューについて",
                "Über diese Vorschau",
            ],
            Text::AboutHint => [
                "Local discovery and a read-only view of Codex settings. Agent files are never changed.",
                "本机 Agent 探测与 Codex 设置只读查看。不会更改 Agent 文件。",
                "本機 Agent 偵測與 Codex 設定唯讀檢視。不會變更 Agent 檔案。",
                "ローカル検出と Codex 設定の表示。エージェントのファイルは変更しません。",
                "Lokale Erkennung und Codex-Einstellungen zum Lesen. Agent-Dateien werden nicht geändert.",
            ],
            Text::LastKnown => [
                "Last readable value",
                "上次可读值",
                "上次可讀值",
                "前回読み取れた値",
                "Zuletzt lesbarer Wert",
            ],
            Text::LastKnownHint => [
                "Showing the last readable model while this configuration needs attention.",
                "当前配置需要检查，暂显示上次可读的模型。",
                "目前設定需要檢查，暫時顯示上次可讀的模型。",
                "設定を確認してください。前回読み取れたモデルを表示しています。",
                "Die Konfiguration muss geprüft werden. Das zuletzt lesbare Modell wird angezeigt.",
            ],
        };
        values[self.index()]
    }

    pub const fn index(self) -> usize {
        match self {
            Self::English => 0,
            Self::SimplifiedChinese => 1,
            Self::TraditionalChinese => 2,
            Self::Japanese => 3,
            Self::German => 4,
        }
    }
}

#[derive(Clone, Copy)]
pub enum Text {
    ConfigPath,
    CopyPath,
    Copied,
    Executable,
    ValidHint,
    MissingHint,
    InvalidHint,
    TooLargeHint,
    Utf8Hint,
    UnreadableHint,
    NotAFileHint,
    SymlinkHint,
    UnavailableHint,
    ErrorLine,
    Agents,
    Settings,
    Intro,
    LocalAgents,
    Refresh,
    Refreshing,
    Preview,
    Installed,
    Model,
    AgentDefault,
    Effort,
    Provider,
    Details,
    HideDetails,
    ConfigReady,
    ConfigMissing,
    ConfigInvalid,
    ConfigLarge,
    ConfigEncoding,
    ConfigUnreadable,
    ConfigNotFile,
    ConfigSymlink,
    ConfigUnavailable,
    ConfigUnsupported,
    UnsupportedHint,
    ConfigSource,
    OtherAgentHint,
    EmptyTitle,
    EmptyHint,
    Language,
    Appearance,
    System,
    Light,
    Dark,
    Preferences,
    About,
    AboutHint,
    LastKnown,
    LastKnownHint,
}

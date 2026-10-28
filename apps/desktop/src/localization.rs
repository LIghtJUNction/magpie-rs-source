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
                "No configuration file exists here. Preview and explicitly save a connection to create it.",
                "此处没有配置文件，可预览并明确保存连接后创建。",
                "此處沒有設定檔，可預覽並明確儲存連線後建立。",
                "設定ファイルはありません。接続をプレビューし、明示的に保存すると作成します。",
                "Hier fehlt die Konfigurationsdatei. Verbindung prüfen und ausdrücklich speichern, um sie zu erstellen.",
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
                "Inspect local tools. Connect Codex with a reviewed configuration change.",
                "查看本地工具，预览配置变更后连接 Codex。",
                "查看本機工具，預覽設定變更後連線 Codex。",
                "ローカルツールを確認し、設定変更を確認して Codex を接続します。",
                "Lokale Tools prüfen. Codex nach Prüfung der Konfigurationsänderung verbinden.",
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
            Text::Installed => [
                "Installed",
                "已安装",
                "已安裝",
                "インストール済み",
                "Installiert",
            ],
            Text::Model => [
                "Model in config",
                "配置中的模型",
                "設定中的模型",
                "設定内のモデル",
                "Modell in der Konfiguration",
            ],
            Text::Effort => ["Reasoning", "推理强度", "推理強度", "推論", "Denkaufwand"],
            Text::Provider => ["Provider", "供应商", "供應商", "プロバイダー", "Anbieter"],
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
                "Executable discovery is available for this Agent. Model editing and connections are not implemented here.",
                "此 Agent 已支持程序发现，尚未实现模型编辑与连接。",
                "此 Agent 已支援程式偵測，尚未實作模型編輯與連線。",
                "このエージェントは実行ファイルの検出のみ対応しています。モデル編集と接続は未実装です。",
                "Programmerkennung ist für diesen Agent verfügbar. Modellbearbeitung und Verbindungen sind noch nicht implementiert.",
            ],
            Text::EmptyTitle => [
                "No installed agents found",
                "未发现已安装的 Agent",
                "未發現已安裝的 Agent",
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
                "Adjust the language and appearance of this window.",
                "调整此窗口的语言与外观。",
                "調整此視窗的語言與外觀。",
                "このウィンドウの言語と外観を変更します。",
                "Sprache und Darstellung dieses Fensters anpassen.",
            ],
            Text::LastKnown => [
                "Last readable data",
                "上次可读数据",
                "上次可讀資料",
                "前回読み取れたデータ",
                "Zuletzt lesbare Daten",
            ],
            Text::LastKnownHint => [
                "The current file could not be read. These values were retained in memory during this session; they may no longer apply.",
                "当前文件无法读取。以下是本次运行中保留的旧值，可能已不再适用。",
                "目前檔案無法讀取。以下是本次執行保留的舊值，可能已不再適用。",
                "現在のファイルは読み取れません。今回はメモリーに保持した以前の値を表示しています。現在の設定と異なる場合があります。",
                "Die aktuelle Datei ist nicht lesbar. Diese Werte stammen aus dem Arbeitsspeicher dieser Sitzung und können veraltet sein.",
            ],
            Text::PickAgent => [
                "Choose an Agent",
                "选择一个 Agent",
                "選擇一個 Agent",
                "エージェントを選択",
                "Agent auswählen",
            ],
            Text::PickAgentHint => [
                "Select a tool on the left to inspect its local information.",
                "从左侧选择工具，查看它的本地信息。",
                "從左側選擇工具，查看它的本機資訊。",
                "左側のツールを選ぶとローカル情報を表示します。",
                "Links ein Tool auswählen, um lokale Informationen anzuzeigen.",
            ],
            Text::BackToAgents => [
                "Back to Agents",
                "返回 Agent",
                "返回 Agent",
                "エージェントに戻る",
                "Zurück zu Agents",
            ],
            Text::NotSet => ["Not set", "未设置", "未設定", "未設定", "Nicht festgelegt"],
            Text::SessionOnly => [
                "Language and appearance apply to this window only. They reset when you reopen Magpie.",
                "语言和外观仅用于此窗口；重新打开 Magpie 后恢复默认。",
                "語言和外觀僅用於此視窗；重新開啟 Magpie 後恢復預設。",
                "言語と外観はこのウィンドウにのみ適用します。再起動すると既定に戻ります。",
                "Sprache und Darstellung gelten nur für dieses Fenster und werden beim Neustart zurückgesetzt.",
            ],
            Text::SupportedAgents => [
                "Detects Claude Code, Gemini CLI, OpenCode, Codex and Zed on PATH.",
                "通过 PATH 检测 Claude Code、Gemini CLI、OpenCode、Codex 和 Zed。",
                "透過 PATH 偵測 Claude Code、Gemini CLI、OpenCode、Codex 和 Zed。",
                "PATH から Claude Code、Gemini CLI、OpenCode、Codex、Zed を検出します。",
                "Erkennt Claude Code, Gemini CLI, OpenCode, Codex und Zed im PATH.",
            ],
            Text::NoReadableValues => [
                "No readable configuration values",
                "暂无可读配置值",
                "暫無可讀設定值",
                "読み取れる設定値がありません",
                "Keine lesbaren Konfigurationswerte",
            ],
            Text::Selected => ["Selected", "已选择", "已選取", "選択済み", "Ausgewählt"],
        };
        values[self.index()]
    }

    pub fn scan_summary(self, count: usize) -> String {
        match self {
            Self::SimplifiedChinese => format!("扫描完成 · {count} 个 Agent"),
            Self::TraditionalChinese => format!("掃描完成 · {count} 個 Agent"),
            Self::Japanese => format!("確認完了 · {count} 件"),
            Self::German => format!("Suche abgeschlossen · {count} Agents"),
            Self::English => format!(
                "Scan complete · {count} {}",
                if count == 1 { "Agent" } else { "Agents" }
            ),
        }
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
    Installed,
    Model,
    Effort,
    Provider,
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
    LastKnown,
    LastKnownHint,
    PickAgent,
    PickAgentHint,
    BackToAgents,
    NotSet,
    SessionOnly,
    SupportedAgents,
    NoReadableValues,
    Selected,
}

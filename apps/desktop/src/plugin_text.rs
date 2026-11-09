//! Localized plugin management text; technical identifiers remain unchanged.
use crate::localization::Locale;

#[derive(Clone, Copy)]
pub(crate) enum PluginText {
    Bundled,
    Local,
    UnknownSource,
    Unsupported,
    Diagnostics,
    DiagnosticsShown,
    DiagnosticsHint,
    Search,
    Enable,
    Disable,
    Off,
    Pending,
    Active,
    Failed,
    Unknown,
    Loading,
    Refresh,
    Error,
    Hint,
    Empty,
    NoMatches,
}

impl Locale {
    pub(crate) fn plugin_text(self, key: PluginText) -> &'static str {
        let values = match key {
            PluginText::Bundled => ["Bundled", "随附", "隨附", "同梱", "Mitgeliefert"],
            PluginText::Local => ["Local", "本地", "本機", "ローカル", "Lokal"],
            PluginText::UnknownSource => [
                "Unknown source",
                "来源未知",
                "來源未知",
                "提供元不明",
                "Unbekannte Quelle",
            ],
            PluginText::Unsupported => [
                "Not supported by this host",
                "宿主不支持",
                "宿主不支援",
                "このホストでは未対応",
                "Von diesem Host nicht unterstützt",
            ],
            PluginText::Diagnostics => [
                "Registration diagnostics",
                "注册诊断",
                "註冊診斷",
                "登録の診断",
                "Registrierungsdiagnose",
            ],
            PluginText::DiagnosticsShown => ["Shown", "已显示", "已顯示", "表示件数", "Angezeigt"],
            PluginText::DiagnosticsHint => [
                "Correct the local plugin package, then refresh. Only source information and static errors are shown.",
                "请修正本地插件包后刷新。这里只显示来源信息与静态错误。",
                "請修正本機外掛程式套件後重新整理。此處僅顯示來源資訊與靜態錯誤。",
                "ローカルのプラグインパッケージを修正して更新してください。提供元情報と固定のエラーのみを表示します。",
                "Das lokale Plugin-Paket korrigieren und dann aktualisieren. Es werden nur Quellenangaben und statische Fehler angezeigt.",
            ],
            PluginText::Search => [
                "Search plugins",
                "搜索插件",
                "搜尋外掛程式",
                "プラグインを検索",
                "Plugins suchen",
            ],
            PluginText::Enable => ["Enable", "启用", "啟用", "有効にする", "Aktivieren"],
            PluginText::Disable => ["Disable", "停用", "停用", "無効にする", "Deaktivieren"],
            PluginText::Off => ["Disabled", "已停用", "已停用", "無効", "Deaktiviert"],
            PluginText::Pending => [
                "Awaiting host or dependencies",
                "等待宿主或依赖",
                "等待宿主或相依項目",
                "ホストまたは依存関係を待機中",
                "Warten auf Host oder Abhängigkeiten",
            ],
            PluginText::Active => ["Active", "已生效", "已生效", "有効", "Aktiv"],
            PluginText::Failed => [
                "Registration failed",
                "注册失败",
                "註冊失敗",
                "登録に失敗",
                "Registrierung fehlgeschlagen",
            ],
            PluginText::Unknown => [
                "Unknown state",
                "状态未知",
                "狀態未知",
                "状態不明",
                "Unbekannter Status",
            ],
            PluginText::Loading => [
                "Reading plugin state…",
                "正在读取插件状态…",
                "正在讀取外掛程式狀態…",
                "プラグインの状態を読み込み中…",
                "Plugin-Status wird gelesen…",
            ],
            PluginText::Refresh => ["Refresh", "刷新", "重新整理", "更新", "Aktualisieren"],
            PluginText::Empty => [
                "No plugins are registered.",
                "还没有已注册的插件。",
                "尚無已註冊的外掛程式。",
                "登録されたプラグインはありません。",
                "Es sind keine Plugins registriert.",
            ],
            PluginText::NoMatches => [
                "No plugins match your search.",
                "没有符合搜索条件的插件。",
                "沒有符合搜尋條件的外掛程式。",
                "検索条件に一致するプラグインはありません。",
                "Keine Plugins entsprechen dieser Suche.",
            ],
            PluginText::Error => [
                "Plugin state could not be read or changed. Refresh to retry. Connected Agents must be disconnected before disabling their adapter.",
                "插件状态读取或修改失败，请刷新后重试。已接入的 Agent 需要先断开，才能停用其适配插件。",
                "無法讀取或變更外掛程式狀態，請重新整理後再試。已連線的 Agent 必須先中斷連線，才能停用其介接外掛程式。",
                "プラグインの状態を読み込み、または変更できませんでした。更新して再試行してください。アダプタープラグインを無効にする前に、接続中の Agent の接続を解除してください。",
                "Der Plugin-Status konnte nicht gelesen oder geändert werden. Zum erneuten Versuch aktualisieren. Verbundene Agents müssen vor dem Deaktivieren ihres Adapters getrennt werden.",
            ],
            PluginText::Hint => [
                "Disabling withdraws new actions and subscriptions; saved providers and request history are retained. Node/npm execution is not available yet.",
                "停用会撤回操作入口和订阅，保留已保存的供应商与请求记录。Node/npm 插件执行尚未开放。",
                "停用會撤回操作入口與訂閱，並保留已儲存的供應商和請求紀錄。目前尚未開放執行 Node/npm 外掛程式。",
                "無効にすると新しい操作とサブスクリプションの登録が解除されます。保存済みのプロバイダーとリクエスト履歴は保持されます。Node/npm プラグインの実行はまだ利用できません。",
                "Beim Deaktivieren werden neue Aktionen und Abonnements zurückgezogen. Gespeicherte Anbieter und der Anfrageverlauf bleiben erhalten. Die Ausführung von Node/npm-Plugins ist noch nicht verfügbar.",
            ],
        };
        values[self.index()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_plugin_states_and_actions_have_five_locale_texts() {
        let keys = [
            PluginText::Bundled,
            PluginText::Local,
            PluginText::UnknownSource,
            PluginText::Unsupported,
            PluginText::Diagnostics,
            PluginText::DiagnosticsShown,
            PluginText::DiagnosticsHint,
            PluginText::Search,
            PluginText::Enable,
            PluginText::Disable,
            PluginText::Off,
            PluginText::Pending,
            PluginText::Active,
            PluginText::Failed,
            PluginText::Unknown,
            PluginText::Loading,
            PluginText::Refresh,
            PluginText::Error,
            PluginText::Hint,
            PluginText::Empty,
            PluginText::NoMatches,
        ];
        for locale in Locale::ALL {
            for key in keys {
                assert!(!locale.plugin_text(key).is_empty());
            }
            assert_ne!(
                locale.plugin_text(PluginText::Unknown),
                locale.plugin_text(PluginText::Off)
            );
            assert_ne!(
                locale.plugin_text(PluginText::Unknown),
                locale.plugin_text(PluginText::Pending)
            );
            assert_ne!(
                locale.plugin_text(PluginText::Unsupported),
                locale.plugin_text(PluginText::Pending)
            );
            assert_ne!(
                locale.plugin_text(PluginText::Bundled),
                locale.plugin_text(PluginText::Local)
            );
            let hint = locale.plugin_text(PluginText::Hint);
            assert!(hint.contains("Node/npm"));
        }
    }
}

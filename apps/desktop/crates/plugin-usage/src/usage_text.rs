#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Locale {
    English,
    SimplifiedChinese,
    TraditionalChinese,
    Japanese,
    German,
}

impl Locale {
    pub(crate) fn parse(code: &str) -> Self {
        let code = code.replace('_', "-").to_ascii_lowercase();
        if code.starts_with("zh-tw") || code.starts_with("zh-hk") || code.starts_with("zh-hant") {
            Self::TraditionalChinese
        } else if code.starts_with("zh") {
            Self::SimplifiedChinese
        } else if code.starts_with("ja") {
            Self::Japanese
        } else if code.starts_with("de") {
            Self::German
        } else {
            Self::English
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum UsageText {
    Title,
    Intro,
    Search,
    Refresh,
    Loading,
    ReadFailed,
    Stale,
    Empty,
    EmptyHint,
    NoMatches,
    Privacy,
    Requests,
    Completed,
    Failed,
    Cancelled,
    All,
    Input,
    Output,
    Total,
    Provider,
    Duration,
    Incomplete,
    Reported,
    CostUnknown,
    Stream,
    Json,
    LocalTime,
}

impl Locale {
    pub(crate) fn usage_text(self, key: UsageText) -> &'static str {
        let values = match key {
            UsageText::Title => ["Usage", "用量", "用量", "使用量", "Nutzung"],
            UsageText::Intro => [
                "The latest 100 recorded forwarded requests.",
                "最近 100 条已记录的转发请求。",
                "最近 100 筆已記錄的轉送請求。",
                "記録された最新100件の転送リクエスト。",
                "Die letzten 100 aufgezeichneten weitergeleiteten Anfragen.",
            ],
            UsageText::Search => [
                "Search provider, model or request ID",
                "搜索供应商、模型或请求 ID",
                "搜尋供應商、模型或請求 ID",
                "プロバイダー、モデル、リクエストIDを検索",
                "Anbieter, Modell oder Anfrage-ID suchen",
            ],
            UsageText::Refresh => ["Refresh", "刷新", "重新整理", "更新", "Aktualisieren"],
            UsageText::Loading => [
                "Reading requests…",
                "正在读取请求…",
                "正在讀取請求…",
                "リクエストを読み込み中…",
                "Anfragen werden gelesen…",
            ],
            UsageText::ReadFailed => [
                "Could not read request history. Check the local data directory and CLI version, then retry.",
                "无法读取请求记录，请检查本地数据目录和 CLI 版本后重试。",
                "無法讀取請求紀錄，請檢查本機資料目錄和 CLI 版本後重試。",
                "履歴を読み込めません。データディレクトリとCLIのバージョンを確認して再試行してください。",
                "Verlauf konnte nicht gelesen werden. Datenverzeichnis und CLI-Version prüfen und erneut versuchen.",
            ],
            UsageText::Stale => [
                "Showing the last successful snapshot.",
                "当前显示上次成功读取的数据。",
                "目前顯示上次成功讀取的資料。",
                "前回読み込んだデータを表示しています。",
                "Die zuletzt erfolgreich gelesenen Daten werden angezeigt.",
            ],
            UsageText::Empty => [
                "No recorded requests",
                "还没有请求记录",
                "尚無請求紀錄",
                "記録されたリクエストはありません",
                "Noch keine aufgezeichneten Anfragen",
            ],
            UsageText::EmptyHint => [
                "Send a model request through this app's Gateway, then refresh.",
                "通过此应用的网关发送模型请求后，点击刷新。",
                "透過此應用程式的閘道傳送模型請求後，點選重新整理。",
                "このアプリのゲートウェイにモデルリクエストを送り、更新してください。",
                "Eine Modellanfrage über das Gateway dieser App senden und danach aktualisieren.",
            ],
            UsageText::NoMatches => [
                "No requests match these filters.",
                "没有符合筛选条件的请求。",
                "沒有符合篩選條件的請求。",
                "条件に一致するリクエストはありません。",
                "Keine Anfragen entsprechen diesen Filtern.",
            ],
            UsageText::Privacy => [
                "Request metadata only. Prompts, replies, headers and keys are not recorded.",
                "仅记录请求元数据，不记录提示词、回复、请求头或密钥。",
                "僅記錄請求中繼資料，不記錄提示詞、回覆、標頭或金鑰。",
                "メタデータのみ記録します。プロンプト、応答本文、ヘッダー、キーは保存しません。",
                "Nur Metadaten. Prompts, Antworttexte, Header und Schlüssel werden nicht gespeichert.",
            ],
            UsageText::Requests => ["Requests", "请求数", "請求數", "リクエスト数", "Anfragen"],
            UsageText::Completed => ["Completed", "已完成", "已完成", "完了", "Abgeschlossen"],
            UsageText::Failed => ["Failed", "失败", "失敗", "失敗", "Fehlgeschlagen"],
            UsageText::Cancelled => ["Cancelled", "已取消", "已取消", "キャンセル", "Abgebrochen"],
            UsageText::All => ["All", "全部", "全部", "すべて", "Alle"],
            UsageText::Input => [
                "Input tokens",
                "输入 token",
                "輸入 token",
                "入力トークン",
                "Eingabe-Token",
            ],
            UsageText::Output => [
                "Output tokens",
                "输出 token",
                "輸出 token",
                "出力トークン",
                "Ausgabe-Token",
            ],
            UsageText::Total => [
                "Total tokens",
                "合计 token",
                "合計 token",
                "合計トークン",
                "Token gesamt",
            ],
            UsageText::Provider => ["Provider", "供应商", "供應商", "プロバイダー", "Anbieter"],
            UsageText::Duration => ["Duration", "耗时", "耗時", "所要時間", "Dauer"],
            UsageText::Incomplete => [
                "Incomplete records",
                "不完整记录",
                "不完整紀錄",
                "不完全な記録",
                "Unvollständige Einträge",
            ],
            UsageText::Reported => ["Reported", "已报告", "已回報", "報告あり", "Gemeldet"],
            UsageText::CostUnknown => [
                "Unknown usage is shown as —. Costs and account quotas are not available yet.",
                "未知用量显示为 —。费用和账户额度暂不可用。",
                "未知用量顯示為 —。費用和帳戶額度尚不可用。",
                "不明な使用量は — で表示します。料金とアカウント割り当ては未対応です。",
                "Unbekannte Nutzung erscheint als —. Kosten und Kontokontingente sind noch nicht verfügbar.",
            ],
            UsageText::Stream => ["Stream", "流式", "串流", "ストリーム", "Stream"],
            UsageText::Json => ["JSON", "JSON", "JSON", "JSON", "JSON"],
            UsageText::LocalTime => [
                "Local time",
                "本地时间",
                "本機時間",
                "ローカル時刻",
                "Lokale Zeit",
            ],
        };
        values[match self {
            Locale::English => 0,
            Locale::SimplifiedChinese => 1,
            Locale::TraditionalChinese => 2,
            Locale::Japanese => 3,
            Locale::German => 4,
        }]
    }
}

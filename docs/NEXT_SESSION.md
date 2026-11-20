# 当前接续进度 · 2026-11-02

此文件记录插件化 checkpoint，完整产品尚未完成。用户再次要求 Agent 功能按原版 1:1、好好设计界面和 Logo；全宽按钮/简陋表单与插件列表未获认可。宽屏 Gateway 侧栏方向曾获有条件认可，窄窗三排顶部导航被否决。底部常驻偏好提示与版本栏已删除，窄窗同侧栏收为 68px 图标栏，宽窗保留 232px 侧栏。

## 已有实现

- 核心保留通用 JSON/JSONC/TOML 配置事务、插件 registry、Provider/凭据、四协议 Gateway 及请求元数据接口。Codex 专属事务/探测和 68 名称目录已移入独立插件；目录数量不代表可用适配器。
- 3 个默认 Agent 配置适配器：Codex、Claude Code、OpenCode；2 个明确版本的旧适配器默认关闭：Kimi CLI 1.52(Python)、OpenManus 固定源码。已做隔离合成配置预览、应用、换模、漂移与恢复，未执行真实 Agent/账户。
- 20 个供应商声明式包贡献 21 个预设，自定义四协议 API 保留。预设数量不是已登录账户、OAuth 支持或真实服务兼容数量。
- Usage 后端/桌面各独立包，Host 注入 handler/page/subscriber；关闭后撤回页面与新订阅，重开保留数据。在途 HTTP 不被关闭 Usage 中断。元数据有界、未知 token 不填零，各字段汇总显示已报告覆盖范围。
- 新 Material 薄壳使用通用插件 Agent 页、Providers/Gateway/Plugins/Settings 及动态页面；旧 Codex 专用大窗、Snapshot/ActionsState 等已删除。Bridge 只保留通用 typed plugin IPC、Provider 与 Gateway 管理，取消/关闭等待实际清理。
- 原配置中的外部模型仍只读显示，不自动选作待接入 Gateway 模型；Codex provider/effort 作为有界、五语 public_fields 返回。秘密和原始配置不出现在这些公开字段。

## 本轮实际验证

- 后端 workspace all-features：416 passed、4 ignored、0 failed；严格 Clippy 与 rustdoc warnings-as-errors 通过。
- Material 桌面 workspace all-features：74 passed、1 ignored、0 failed；严格 all-target/all-features Clippy 通过。删除了不再使用的旧桌面测试，不将旧 80 项结果套到新代码。
- 生产壳通过独立 render_preview 导出合成场景，像素与实际目视复核分开；当前日志位于外部 TASK/plugin-visual-2 与 logs/plugin-checkpoint-*。
- 本轮重新运行 Linux Wayland 隔离窗口，实物检查宽屏和约 440px 图标栏、单行顶栏、底栏删除。完整新版 Provider/Agent/插件启停的原生操作和其余尺寸仍须继续验证，组件或像素数量不能代替。
- CLI/headless 不启动 GPU/显示；测试不读取真实账户或 Agent 会话。没有发布软件包、AUR 安装或新的跨平台执行证据。全部 source checkpoint 使用 [skip ci]。

## 当前重点

1. Agent 页按原版完整逐项对照：紧凑行、版本/状态、行内模型及可见范围、新会话默认、子代理/上下文/压缩/辅助模型等专属设置；真实写入与版本门槛在插件实现，精确恢复和 revision 保持。供应商添加也重新设计。
2. 设置支持常规、额度、网络与共享、模型、隐私、可观测性、同步与备份、关于；语言和主题按钮；组件可拖动排序、增减并保存布局。
3. 插件中心内置/发现/已安装，搜索/npm/GitHub/本地安装与 schema 配置，真实运行/依赖/登录状态。当前声明式/已链接 Rust 插件不等于 Node/Bun 社区宿主、OAuth 或 MCP。
4. 路由可视化、脱敏、统计、资源库、会话各自独立可启停。脱敏要真实出站替换/流式回复还原，关闭后在途上下文仍有效；会话读取按 Agent 许可且录制默认关闭。
5. 应用品牌重新设计，Agent/供应商使用有来源和适当许可的对应官方标识，适配浅深背景与小尺寸；不以通用图标冒充品牌。

配方升级到 1.5.0，保留全部 99 REQ；最新细化见 PLUGINS.md、SETTINGS_AND_PLUGIN_UI.md、ISSUE_FEEDBACK.md(F03–05)。此前 1.4.1、329 tests 和旧像素/原生结果都是历史证据，不代表当前所有路径重验。

任务目录 /home/lightjunction/.cache/magpie-rs-build/smoke-20261102/；构建不在 checkout。脚本 scripts/desktop-fixture.py --cli PATH --desktop PATH --output NEW_DIR 启动合成 loopback 窗口，--reuse 复用自身 fixture；真实认证目录被 bubblewrap 隔离。桌面编译使用已安装 Rust 1.99.1（默认 1.99.0 不满足 Material MSRV）。先提交推送源仓库，再更新父仓库 gitlink；继续保留无关改动，不重置/清理工作区。

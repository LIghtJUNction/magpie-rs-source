# 更新日志驱动的功能与 UI 迭代

日期：2026-10-30 UTC。软件版本仍为开发预览 0.2.0；对应父配方升级为 1.4.0。
本轮先按用户指示合并上一轮源码 PR #2 与父配方 PR #1，再更新完整需求、继续实现并逐项审计。
源码起点为 `338c9328428d95c5cc776c04b713c09c7b9c5d28`，父仓起点为
`487eda5ef4afce9b96d1305e8d9252bdd05ee828`。本报告的验证范围由机器记录中的输入 SHA-256
限定，不把历史测试或参考软件的结果当作新代码已经通过。

## 原版基线与配方修正

本轮固定原版 **0.1.2185**，源码
[`f1151226262022743bd103a78af41e1004cc6055`](https://github.com/yetone/magpie/commit/f1151226262022743bd103a78af41e1004cc6055)。
完整阅读官方 [notes 区间](https://usemagpie.ai/api/notes?after=0.1.1947&upto=0.1.2185)，
包含 1948 基线和其后 236 条发布，共 237 条；官方区间没有 2070。
相对此前查看过的 `dca3e2d716fe21bbb3e239f769a8e7861b135c01` 源码，新增发布是 37 条。
没有运行 2185 的原版 GUI，也没有使用真实供应商账户。

父配方 `UPSTREAM_CHANGELOG.md` 保留逐发布分类、需求映射、来源版本和原始材料散列。
除新增 REQ-105–110，旧合同还修正了若干遗漏：Responses WebSocket/TTS 的旧排除失效；
Ollama 原生 Chat/Generate 在旧原版中已经存在；Request archive 的 1 GiB 与 Sessions
录制的七天/256 MiB 是独立设置；只读 key 的消费权限与额度数据可见范围不能合并。
公开 Gateway 路由另按固定源码登记，不能因为旧表没有列出 embeddings、rerank、图片或
视频入口，就将其排除在完整重建目标之外。

日志摘要不总能准确表述实际条件。例如 Scripts 合并只影响 Context 请求来源卡，
不会增删已安装 Agent 目录；2155 的预先改 token 字段只适用于 OpenAI 自有域名。
这些差异均回到固定原版源码和测试确认。

## 本轮实际改进

### 桌面 Gateway 生命周期

新增原生 Gateway 页面，与现有紧凑列表、颜色、语言和响应式导航一致。它显示真实
监听地址、服务端报告的版本、配置模型数量和运行/失败状态，提供启动、停止、检查和复制
连接地址。桌面保存本机监听地址，旧偏好文件继续默认 127.0.0.1:3425；损坏偏好不会
悄悄退到默认值并启动服务。独立 CLI 的地址仍由 `--listen` 指定，尚不是完整共享设置系统。

桌面默认尝试启动自己的服务。已有可识别的外部服务保持只读，不能停止；公开端口、
版本或 instance ID 均不是所有权证明。实际所有权来自本程序启动的 child handle 和私有
stdin 管道。`serve --supervised --json` 完成服务初始化后输出一次 ready，桌面再检查
健康与目录的身份是否一致。长期服务不占原短 CLI 命令的单通道和 20 秒期限。

正常停止先关闭自有管道，超时才结束自己的 child。持有管道的桌面进程异常退出时，
OS 关闭管道使 Gateway 自行停止。关闭窗口等待其他保存任务期间禁止再次启动新服务；
后台偏好重载不能把新地址冒充运行中的地址。进入 Gateway 页面会重新读取实际模型目录。
停止服务、改地址或启动失败均不修改 Agent 配置。Web/TUI/tray 生命周期仍未实现。

入口：[CLI supervised 参数](../crates/magpie-cli/src/main.rs)、
[服务生命周期](../crates/magpie-service/src/forward/lifecycle.rs)、
[桌面进程管理](../apps/desktop/src/cli_bridge/owned_gateway.rs)、
[Gateway 页面](../apps/desktop/src/gateway_page.rs)。

### 已接入 Codex 直接更换模型

新增 `agents set-model codex` 和对应桌面模型选择/预览/保存流程。更新只改变当前 model，
沿用实际托管的 Gateway；不顺带改 Provider、headers 或推理强度。连续从 B 换到 C、D 后
断开，仍恢复首次接入之前的 A。用户在预览之前作出的无关修改保留。

只读预览返回 revision；桌面 apply 带此值，拒绝预览之后的模型更新、无关字段修改或
同内容文件替换。revision 为绑定连接代次、完整配置字节与文件身份的 SHA-256；核心
提交仍检查完整快照与 journal，不只依赖摘要。模型相同是无写入的 no-op。apply 只验证
现有 Gateway 的配置模型目录，不因此宣称供应商模型已在线。

更新引入 `update-prepared` 日志和私有前后映像。恢复只处理精确可识别状态，遇第三种
外部内容保留现场并报告 recovery-required。第一次成功换模后 journal 使用 v2；本版本
读取旧 v1，旧程序遇 v2 会拒绝，以免按错误的 ownership 映像恢复。该迁移只属于事务
格式，不是清理用户配置或登录数据的理由。

用户自行写的同名 `magpie` Provider 没有 ownership journal 时仍是 unmanaged，更新和
断开均不接管它。目录刷新没有 Agent 配置写入旁路。其他 Agent 的配置写入、Profile 和
可见模型列表仍需实现。

入口：[更新 CLI](../crates/magpie-cli/src/codex_update.rs)、
[核心事务](../crates/magpie-core/src/codex_transaction.rs)、
[CLI 行为测试](../crates/magpie-cli/tests/codex_model_update.rs)、
[事务与中断测试](../crates/magpie-core/tests/codex_transaction.rs)。

### Chat 输出上限兼容

依据原版 [0.1.2155 提交](https://github.com/yetone/magpie/commit/cae785a37aacff21fb84089ec80809240d7d61da)：
真正的 `openai.com` 及其子域预先使用 `max_completion_tokens`；其他兼容服务保持原字段。
只有 Chat HTTP 400 明确拒绝当前字段并要求另一字段时，才在同一总 deadline 和并发槽内
重发一次。顶层字段之外的已序列化请求字节保持相同，诊断体最多 16 KiB。

域名伪装、数值上限错误、模糊或矛盾诊断、双字段冲突、无字段、非 Chat、401/429/5xx、
第二次 400 和已经开始的 SSE 均不触发额外重放。取消能结束诊断读取和上游连接。
未加入原版跨请求记忆 Provider 参数偏好的优化，也没有新增独立模型 probe 产品流程。
这不是通用 routing retry/fallback、完整上游字段兼容或 Usage 账本。

入口：[token 兼容模块](../crates/magpie-service/src/forward/token_field.rs)、
[真实 HTTP/SSE fixture](../crates/magpie-service/tests/forward_protocol_http.rs)。

### 目录模式与转发模式使用一致的本机访问检查

旧 `serve --catalog` 只检查 Host 数量及非空。本轮补上真实 peer、精确监听 Host、Origin、
Forwarded 和 X-Forwarded-* 检查，和转发模式共用规则。非法浏览器/代理请求不能利用
loopback 监听免认证；允许范围内的 GET/HEAD 和目录工作线程继续正常工作。
这不代表已经实现 LAN 分享、远程 Gateway key 或 Web 登录。

入口：[共享规则](../crates/magpie-service/src/local_access.rs)、
[目录 HTTP 测试](../crates/magpie-service/tests/catalog_http.rs)。

## 99 条需求逐项审计的结果

完整旧需求的逐项证据分为两份：

- [Core/Provider/Gateway/路由与安全 46 项](feature-audit-core-2026-10-30.md)。
- [用户界面、Usage、Library、插件与多入口 47 项](feature-audit-interfaces-2026-10-30.md)。

这两份是本轮修改前的源码基线审计；本报告记录其后已经补上的行为，父配方 COVERAGE
保存当前结论。每条均保留实际入口、实现边界、既有证据和下一条验收。新增六条的当前
实现判断如下，不因相邻组件存在而升级：

| REQ | 新能力 | 当前实现判断 |
| --- | --- | --- |
| 105 | Library MCP Hub | 无聚合路由、Library 后端管理和 Agent 范围工具调用。 |
| 106 | Responses 续接 | 原生透传可能保留上游字段，但无本机调用者隔离历史、还原、淘汰或跨 Provider 续接。 |
| 107 | TTS | `audio/speech` 不在生成入口登记中；音频/SSE/厂商 JSON 流程未实现。 |
| 108 | 只读 Gateway key | 无 Gateway key 权限存储及路由 allowlist；loopback 免 key 不等于此能力。 |
| 109 | Agent 会话历史许可 | 无 Sessions 读取与许可/撤销/迁移流程；当前不读取会话只是默认边界。 |
| 110 | Responses WebSocket | upgrade 仍明确返回 426；没有多 turn、interrupt、临时历史或每 turn 鉴权。 |

| 功能域 | 需求数 | 部分实现 | 仅组件/前置 | 未实现 |
| --- | ---: | ---: | ---: | ---: |
| Agents 与配置 | 15 | 11 | 1 | 3 |
| Provider 与账户 | 10 | 4 | 0 | 6 |
| Gateway 与新增协议 | 16 | 7 | 0 | 9 |
| 路由 | 10 | 0 | 4 | 6 |
| Usage 与 Sessions | 10 | 0 | 4 | 6 |
| Plugins | 10 | 0 | 0 | 10 |
| Library、同步与备份 | 10 | 1 | 1 | 8 |
| 运行入口与设置 | 7 | 5 | 0 | 2 |
| 安全与平台 | 6 | 6 | 0 | 0 |
| UI 与语言 | 5 | 5 | 0 | 0 |
| **合计** | **99** | **39** | **10** | **50** |

“部分”表示已有可使用的子流程；“仅组件”表示没有贯通该产品工作流。当前没有一条达到
完整合同规定的全部入口、平台与真实外部兼容验收，因此不计算完成百分比。尤其不能把
68 个 Agent 目录条目当成 68 个可写配置适配器，或把七种路由纯计算当成实际网关选路。

## 验证记录

当前 Linux x86_64 环境使用 Rust 1.99.1、锁定依赖、独立 GPUI workspace 和合成配置。
所有服务请求均发到测试拥有的 loopback fixture；没有访问真实 Agent 账号、Provider
密钥、真实 HOME 或会话历史。机器明细见
[changelog-parity-2026-10-30.json](../tests/results/changelog-parity-2026-10-30.json)。

| 检查 | 结果和边界 |
| --- | --- |
| workspace 全 targets | 327 passed，0 failed；4 个默认忽略的项是由父测试显式启动的隔离 worker。 |
| workspace Clippy | `--workspace --all-targets -- -D warnings` 通过。 |
| workspace 格式 | `cargo fmt --all --check` 通过。 |
| 真实监督 CLI | ready/health/inspect 一致、端口冲突、损坏 store、EOF 退出与重新启动通过。 |
| 持有 lease 的进程异常退出 | 实际结束独立测试 owner，Gateway 自行正常退出、拒绝新连接且端口可重新绑定；未对 Gateway 发送 kill。 |
| 桌面组件 | 78 passed，0 failed；3 个默认忽略入口中，两条新实际 CLI 流程另行显式执行并通过，既有安装版 Codex worker 本轮未重跑。 |
| 实际桌面 CLI 联动 | 自有 Gateway 启停/外部只读；连接 → 已接入换模 → 沿用同一 Gateway → 停止 → 离线断开恢复原字节，两条均通过。 |
| 桌面 Clippy / 格式 | 独立 workspace、visual-test、全 targets `-D warnings` 与 fmt 通过。 |
| 原生 Scene | 中文 10 个场景 × 4 种尺寸 × 浅深主题，共 80 场景；1×/2× 共 160 PNG 全部实际生成，尺寸与 manifest 一致。 |
| 实际目视复核 | 5 个最终字节一致的代表场景，主审直接查看其中 4 个；没有把 80 个成功渲染都写成目视通过。 |

目视检查覆盖宽窗浅色 Gateway、360px 深色外部服务、360px 错误状态及 360/900px 换模预览。
检查后减少 Gateway 的多余顶部留白，更新预览改为同时呈现目标模型、沿用的 Gateway 和
“保存模型”，配置路径及字段收进详情。最终 PNG 与已目视版本逐一 SHA-256 相同。
窄窗 Gateway 的部分地址表单位于首屏下方的滚动区域；截图不证明所有控件同时可见，
也不代替真实窗口滚动与键盘验收。后台探测的有界清理与 CleanupPending 门禁另由组件
和实际 CLI 测试验证，保存地址/偏好重载不能绕过尚未完成的清理。

复现核心验证：

```sh
cargo test --locked --workspace --all-targets
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo fmt --all --check
python3 scripts/verify-supervised-gateway.py --magpie /absolute/path/to/magpie
```

首次故障注入回归把第 2 次目录同步误作 journal 发布窗口；实际它属于 recovery image
预发布，返回 Write 正确。已定位到真正的第 3/4 次同步并保留状态、错误和恢复断言，
最终全套通过。独立 owner-crash 检查也明确区分 TCP TIME_WAIT 和仍在监听，使用连接
拒绝及允许复用地址的重绑定共同验证。共享构建缓存中的 CLI 曾被另一构建替换，因此
最终进程验收使用本 checkout 输出到独立目录的二进制，并记录其 SHA-256。

## 尚未验收的范围

327 项测试不代表原版全部功能完成。完整 Provider 预设、订阅、多账户、路由接线、
计量/Usage/Sessions、插件、Library/同步/备份、Web/TUI/tray，以及本文六项新增能力均
按逐条审计继续保留。token-count、完整目录形态、embedding/rerank、图片/视频、音频、
Ollama 原生接口和列举的别名也不能由四协议对话 fixture 替代。

原生 Scene 渲染不等于真实窗口、OS 读屏/IME 或用户视觉认可。macOS/Windows/Android、
各 CPU、真实供应商与其他 Agent 需要分别验证；本轮没有执行新 Release、AUR 发布或
用户系统安装。Windows 配置写入和 Android 原生 GPUI 后端的既有缺口继续明确保留。

# Core、Provider、Gateway 与路由逐项功能审计

审计日期：2026-10-30。范围：REQ-010–049、REQ-090–095，共 **46 条**。
本文件逐项核对完整规格、正常/失败验收、真实用户入口、实现与已有证据；不把函数、
字段、页面或测试数量当作整条需求通过。本次没有运行 Cargo、外部账户或新的产品验收。

## 基线和证据口径

- 源码合并提交：`338c9328428d95c5cc776c04b713c09c7b9c5d28`；其 tree
  `b414809af6585abdeba9c1e925d5e57f8f74869d` 与实现提交 `452d59e` 相同。
- 配方基线：父仓库 `487eda5ef4afce9b96d1305e8d9252bdd05ee828`。读取了
  `recipes/magpie-core/specs/01-product-and-agents.md`、`02-providers-and-accounts.md`、
  `04-routing-and-failover.md`、`09-privacy-security-and-platforms.md`，
  `recipes/magpie-gateway/specs/03-gateway-and-protocols.md`，两组件的 BUILD_PROMPT、
  ACCEPTANCE，以及根验收、覆盖表和原版隔离 Web 观察。后续更新日志带来的配方新增项
  应另列增量，不能倒推本次源码已实现。
- 当前覆盖表 SHA-256：`5f38d8933e9dc7086f05e8a0ff29b5d04a10164330b5081b4d0953183f73b1f2`。
- 对 [2026-10-30 机器证据](../tests/results/provider-ui-parity-2026-10-30.json)
  的 35 个 `input_sha256` 作只读核对，全部与本次源码一致。路由旧记录的
  `routing.rs`/路由测试指纹仍一致，`lib.rs` 因新模块导出发生变化；旧记录仍按其版本解释。
- 本范围内没有一条可以标为完整通过。下文“部分”只指明确列出的实际子流程；
  “仅组件”表示没有用户可调用的完整路径；“未实现”不因邻接组件存在而升级。

### 实现位置索引

| 索引 | 实际文件和职责 |
| --- | --- |
| S0 | [CLI 分派](../crates/magpie-cli/src/main.rs)、[桌面启动](../apps/desktop/src/main.rs)、[桌面 UI](../apps/desktop/src/ui.rs)：可达命令/页面与生命周期 |
| SA | [Agent 注册表](../crates/magpie-core/src/agent_catalog.rs)、[发现](../crates/magpie-core/src/agents.rs)、[Codex 只读配置](../crates/magpie-core/src/codex_config.rs) |
| ST | [Codex 事务](../crates/magpie-core/src/codex_transaction.rs)、[桌面动作状态](../apps/desktop/src/actions_state.rs)、[快照](../apps/desktop/src/snapshot.rs)、[CLI bridge](../apps/desktop/src/cli_bridge.rs) |
| SP | [ProviderStore](../crates/magpie-core/src/provider_store.rs)、[凭据事务](../crates/magpie-core/src/credentials.rs)、[Provider CLI](../crates/magpie-cli/src/providers.rs)、[Provider 页面](../apps/desktop/src/providers.rs) |
| SC | [模型刷新](../crates/magpie-service/src/provider_catalog.rs)、[实时 Provider 解析与目录](../crates/magpie-service/src/forward/source.rs)、[旧公共目录](../crates/magpie-core/src/catalog.rs) |
| SG | [转发服务](../crates/magpie-service/src/forward.rs)、[旧目录服务](../crates/magpie-service/src/gateway.rs)、[旧 HTTP 解析](../crates/magpie-service/src/http.rs) |
| SX | [协议转换](../crates/magpie-service/src/protocol.rs)及其 [模块说明](../crates/magpie-service/src/protocol/README.md) |
| SR | [纯内存路由](../crates/magpie-core/src/routing.rs)：Group/Router/Snapshot/Decision，不执行 I/O |

### 既有验收记录索引

| 索引 | 记录及可证明的范围 |
| --- | --- |
| E1 | [本轮机器记录](../tests/results/provider-ui-parity-2026-10-30.json)：Linux workspace 298 passed、4 个辅助入口 ignored；桌面 68 passed、1 个辅助入口 ignored。包含源码中的具体测试；不代表完整原版或其他系统通过 |
| E2 | E1 的 `real_secret_service`，对应 [隔离验收脚本](../scripts/verify-provider-keyring.py)：7 个工作流检查，真实独立 Secret Service、CLI/Gateway 跨进程、合成凭据/HTTP、轮换/停用/清理和 Codex 精确恢复 |
| E3 | [路由记录](../tests/results/routing-core-2026-10-29.json)和 [36 个路由测试](../crates/magpie-core/tests/routing.rs)：纯计算、合成账户观察，不含持久化或上游请求 |
| E4 | E1 内的 [Codex 事务测试](../crates/magpie-core/tests/codex_transaction.rs)、[CLI/桌面快照测试](../crates/magpie-cli/tests/desktop_snapshot.rs)、ST 中动作状态测试；[历史锁修复记录](../tests/results/codex-lock-release-2026-10-29.json)限定为 Linux |
| E5 | [历史 first-loop 记录](../tests/results/first-loop-2026-10-29.json)和 [脚本](../tests/first_loop.py)：指定旧二进制的 HTTP、Host/Origin、并发、取消、错误脱敏等；本次未重跑该 Python 套件 |
| E6 | [历史原生 Codex](../tests/results/native-codex-2026-10-29.json)、[历史桌面动作/Codex](../tests/results/desktop-actions-2026-10-29.json)：隔离配置、已安装 Codex 0.165.0、合成上游；不等于新版本原生窗口或真实账户验证 |
| E7 | [发现记录](../tests/results/agent-discovery-2026-10-29.json)、[注册表测试](../crates/magpie-core/tests/agent_catalog.rs)：68 个目录项、已核验 PATH 候选及异常；命中不证明 Agent 配置/请求兼容 |
| E8 | E1 内 [HTTP/SSE 转发测试](../crates/magpie-service/tests/forward_protocol_http.rs)、[模型发现测试](../crates/magpie-service/tests/provider_catalog_http.rs)、[公共目录测试](../crates/magpie-service/tests/catalog_http.rs)：合成 loopback 上游 |
| E9 | E1 内 [ProviderStore 测试](../crates/magpie-core/tests/provider_store.rs)、[Provider CLI 测试](../crates/magpie-cli/tests/providers_cli.rs)、SP 的凭据与 GPUI 动作测试 |

## 覆盖表最需要修正的表达

1. **REQ-014/016：首次接入成功不能代表已经接入后可直接切换模型。**
   `ActionsState::can_preview` 只允许 `Ownership::Unmanaged`；事务对已有 Connected
   记录返回 `AlreadyManaged`。当前需要先断开再接入，尚无保留首次恢复点的模型更新事务。
2. **REQ-031/034：缺少的是明确的 API 入口，不只是“全部字段兼容未验收”。**
   `count_tokens`、Gemini `countTokens`、音频、Codex/Gemini 专用目录、Ollama 目录均未接线。
   已有 HTTP 测试明确断言两个 token-count 入口返回 501。
3. **REQ-041：Free first / Manual 在本源码中尚未实现。**
   SR 的 Strategy 枚举只有七种；不能只写“实际网关语义未验收”，让读者误以为已有实现。
4. **REQ-035/091：防护有模式边界。** 默认 ProviderStore 转发路径会检查真实 peer、
   Host、Origin 和转发头。旧 `serve --catalog` 的 `GatewayServer`/`http.rs` 只检查 Host
   数量及非空，没有复用地址/Origin/proxy 校验。后者虽只读目录，也不能计作统一入口防护。
5. **REQ-091 的“未执行”漏记局部实测；REQ-092/095 也有可复用基础。**
   转发的本机访问拒绝、有界并发、错误脱敏、仅选中 Provider 认证材料已有证据。
   这不能提升为远程 key、Privacy 设置或工具隐私完成。
6. **REQ-015/018/026/027/047 的邻接组件须分别记账。** effort 读取/保留不等于 effort 编辑；
   保留 Codex 的既有 `[profiles]` 不等于 Magpie Profiles；Router 输入类型不等于 quota/account
   产品；Decision purpose 隔离不等于 System One 服务。
7. **REQ-022/034 尚无持久化的 stale/最近成功时间。** 刷新失败保留模型确已实现，
   但 `configured`/`not_probed` 不是目录新鲜度，不能据此宣称 stale 契约完成。

## REQ-010–019：Agent 与共享状态

### REQ-010 · 多个入口反映同一状态

- **入口与实际行为（部分）：** CLI `providers`/`models`/Codex 事务、GPUI Agents/Providers
  与 Gateway 共用 SP/ST 的保存状态。Provider 更改影响下一请求；手动刷新能重读配置。
- **证据：** E2 的独立 CLI/Gateway 工作流；E4 的
  `desktop_snapshot_and_cli_discovery_share_committed_and_restored_configuration`；E9 的原生表单动作。
- **缺口：** 没有托盘快速面板、Web/TUI、开机启动；Profile/路由共享未接线；桌面不拥有服务。
  原生窗口之间的状态/失败反馈尚未实际操作验收。
- **下一条验收：** 隔离目录中由 CLI 修改模型，已打开 GPUI 刷新后显示同值；制造读取失败，
  页面保留可识别的旧值并显示失败，不显示保存成功；随后恢复读取。托盘/Web/TUI仍另列待验收。

### REQ-011 · 发现和列出 Agent

- **入口与实际行为（部分）：** SA 的 68 项注册表保留完整对象；CLI/GPUI 采用有界 PATH
  元数据扫描。已核验候选保留原五项并增加八项；只有 Codex 读取模型/Provider/effort。
- **证据：** E7；`declared_contracts_never_grant_configuration_support`、
  `actual_projection_does_not_promote_a_hit_after_an_unreadable_earlier_candidate` 等。
- **缺口：** 其余目录项大多不是完整安装探测或配置适配器；App bundle、Profile、WSL、
  各 Agent 专属目录和操作能力未完整覆盖。发现错误的完整 UI 说明也未逐平台验收。
- **下一条验收：** 选定第二个 Agent，分别提供未安装、已安装无配置、损坏/不可读配置，
  验证行身份、状态与可执行操作明确区分；清空 PATH 不能把扫描失败当成全部未安装。

### REQ-012 · 兼容 Agent 的公开配置格式

- **入口与实际行为（部分）：** ST 只对 Codex TOML 提供实际预览/写入/恢复；SA 读取
  Codex 顶层三个字段。其它 Agent 目前没有对应 JSON/JSONC/YAML 等配置写入路径。
- **证据：** E4 的注释、未知字段、格式保留及非法 TOML拒绝；E6 的特定 Codex 客户端实跑。
- **缺口：** 正常验收要求的 Claude Code JSON、OpenCode JSONC 尚未实现；原生模型目录、
  版本识别、各系统路径与完整 68 类适配仍缺。
- **下一条验收：** 在固定版本 Claude Code 或 OpenCode 的隔离配置中接入一个模型，
  原生选择器可见且实际请求到合成 Gateway；未知字段和注释在断开后保留。

### REQ-013 · 只做精确、可恢复的字段编辑

- **入口与实际行为（部分）：** Codex 事务只管理七个字段，保存初始字节和写入映像；
  同文件系统私有暂存、锁、重读冲突判断、原子替换与按归属恢复已接线。
- **证据：** E4 的精确字节恢复、外部 Header/未知字段保留、同字节换 inode 冲突、
  symlink/只读/大文件/损坏状态/中断恢复测试。
- **缺口：** 只覆盖 Codex 与已支持的 Unix 写入路径；重复 key 的无效 TOML采取拒绝，
  并未提供任意无效文件的字段级修复。外部非协作写入仍有最后检查到 rename 的竞争窗口。
- **下一条验收：** 第二个真实 Agent 格式也通过“仅改目标字段、非目标内容不丢”的相同用例；
  同时以该 Agent 实际覆写进程验证有界冲突拒绝。不能把当前 Linux fixture 当作断电证明。

### REQ-014 · 接入、漂移和断开

- **入口与实际行为（部分）：** CLI/GPUI 可首次连接 Codex、读取归属/漂移，并显式离线断开；
  Disconnect 保留已被用户修改的托管值和新增 Header。连接前检查监听/模型已配置。
- **证据：** E4、E2；E6 两种初始配置状态的历史真实 Codex/恢复结果。
- **缺口：** 已 Connected 状态拒绝再连接，没有直接改模型或重新应用的事务；Profile
  分区、其他 Agent、原生窗口及连接后重启提示的完整流程未验收。
- **下一条验收：** 先连接 A，再在同一托管连接切换 B；首次用户恢复点不变，断开仍恢复首次
  模型；切换期间外部改过模型时拒绝，不覆盖用户值；无关 Header 始终保留。

### REQ-015 · Agent 专属字段和 Profile 保留

- **入口与实际行为（仅组件）：** Codex `model_reasoning_effort` 可读取展示；连接/断开不
  改它，且保留原配置的 `[profiles]`。没有 effort 等字段的编辑入口或 Magpie Profile。
- **证据：** E4 的快照测试保留 medium effort；
  `disconnect_preserves_unrelated_header_profile_unknown_fields_and_comments`。
- **缺口：** 目标版本/Provider 能力驱动的选项、default/恢复、失效选择处理、Profile
  保存/能力校验均未实现。旧“未执行”可以保留产品结论，但应备注读取/保留已有证据。
- **下一条验收：** 为固定版本 Codex 增加真实 effort 选项，非法等级或不支持模型拒绝保存；
  default 删除自有覆盖值，停用 Provider 后失效项不可继续应用，原用户值可恢复。

### REQ-016 · 跨 Agent 模型目录

- **入口与实际行为（部分）：** SC/SP 从启用 Provider 生成 `provider/model`；CLI、Gateway
  和 Codex GPUI Picker 使用该目录，支持搜索。同名模型用 Provider 前缀区分。
- **证据：** E8 `store_routes_two_providers_and_applies_edits_without_restart`；E9 CRUD/目录；
  ST 的移除模型阻止预览和 stale 目录状态测试。
- **缺口：** 无第二 Agent 写入、路由组、能力/effort/上下文/价格与家族筛选；Codex 已托管
  后不能直接切换。ASCII 写入约束也使显示可见不等于所有目录 ID 均能应用。
- **下一条验收：** A/B 两个 Provider 提供同名模型，在两个支持的 Agent 分别选用；原生配置
  和真实上游请求归属正确，已连接 Agent 切换不用断开，明确自定义模型名不被重写。

### REQ-017 · 每 Agent 模型可见性

- **入口与实际行为（未实现）：** 无 Agent 级可见性存储或 CLI/GUI入口；当前目录只有
  Provider 全局 enabled 与 model 列表。停用 Provider 同时阻止新请求，不是 Picker 隐藏。
- **证据：** S0/SP/SC 的实际 schema 和分派；无本需求专用验收记录。
- **缺口：** 模型/Provider/家族/组隐藏、only-picked、`--show-new` 与损坏配置保留策略均缺。
- **下一条验收：** 只对 Codex 隐藏 A，另一个 Agent 仍可见；显式请求 A 成功；新增 B 在
  only-picked 下不自动出现；无法读取过滤配置时不扩大或清空可见集合。

### REQ-018 · 保存、应用和删除 Profile

- **入口与实际行为（未实现）：** 无 `profiles` 命令、Magpie Profile 数据/页面/托盘入口。
  ST 保留用户原来的 Codex Profile，不执行 Magpie 的跨 Agent 快照。
- **证据：** S0 分派；E4 只证明不破坏已有 `[profiles]`，不证明本需求。
- **缺口：** 保存/覆盖/应用/删除、当前 Profile、部分失败与恢复、卸载 Agent 保留值全部缺。
- **下一条验收：** 保存 Work 后改两个 Agent，再预览/应用 Work；一份目标只读时逐项报告
  成功/失败与恢复状态，不能全成功；卸载再重装一个 Agent 后仍能恢复其已保存项。

### REQ-019 · 变化重扫和状态反馈

- **入口与实际行为（部分）：** GPUI 手动刷新/F5重读 SA/ST；坏读取保留上次可读值并标记
  `last_known`，确实删除的配置不保留旧托管值；异步保存和冲突有反馈。
- **证据：** E4 的 `failed_read_retains_old_values_with_stale_marker`、
  `restored_missing_file_has_no_old_managed_model`；ActionsState 预览失效/重读/不确定结果测试。
- **缺口：** 未实现文件变化监听或自动重试；完整 Agent 集合、真实窗口焦点/重启提示未验收。
- **下一条验收：** 外部把 Codex 文件短暂写坏后改成 B，重扫期间保留有 stale 标记的 A，
  下一次读取显示 B；配置被真正删除时清除旧模型而不伪造默认值。

## REQ-020–029：Provider、账户和目录

### REQ-020 · Provider 目录和新增

- **入口与实际行为（部分）：** `providers add/edit/...` 与原生 Providers 页可管理自定义
  单协议 Provider，支持四协议、直接系统 key、显式环境引用及无 key 的本机服务。
- **证据：** E9 的 Store/CLI/GPUI 创建启停删除；E2 的系统 key；表单保存不出网。
- **缺口：** 没有预设目录/搜索分组/Region；DeepSeek 仍不能只填 key。单 base+protocol
  不等于多 endpoint；System One、订阅/插件/Remote类型、尾码与使用中 Agent 标记缺失。
- **下一条验收：** 从共享预设目录选择 DeepSeek，仅输入合成 key即可得到正确默认配置；
  Ollama 不强制 key；未知预设不猜 URL；保存全过程零出网，随后显式测试/刷新才请求 fixture。

### REQ-021 · 多个上游密钥和 Provider 编辑

- **入口与实际行为（部分）：** 每 Provider 一份 credential_ref 或 api_key_env；正常编辑
  保留凭据，可轮换/清除/停用/删除，下一请求读新值，失败尽量回收未发布 key。
- **证据：** E2 的跨进程轮换；SP 的 put 写后报错/CAS冲突/清理失败测试；E9。
- **缺口：** 多 key/account 身份与排序/暂停、历史归属、低成本请求测试、Headers/body/
  代理/并发/速率/队列/价格/fallback/多端点等完整高级字段缺失。
- **下一条验收：** 为同一 Provider 配两份稳定 key身份，轮换第一份后新请求使用新 key，
  历史仍归旧身份；第二份不变；401不删除配置。请求测试和模型刷新分别操作、分别报告。

### REQ-022 · 模型列表和目录元数据

- **入口与实际行为（部分）：** `providers refresh ID`/原生 Refresh 明确出网；四协议 GET
  目录、Anthropic/Gemini分页、去重和完整校验后CAS保存，仅更新 models；可手工编辑列表。
- **证据：** E8 的 9 个 model catalog用例，覆盖401/坏JSON/空/不完整分页/过大/冲突；E2。
- **缺口：** 无公共能力/价格目录、覆盖/重置、glob/regex、only-picked等模式、自动周期；
  没有持久化成功时间/stale；刷新直接替换列表，不代表多种暴露规则已实现。
- **下一条验收：** 配置“仅挑选”与“按模式自动包含”两个 Provider，刷新新增模型后分别按
  规则出现；上游离线时保留最后列表和成功时间，并可见 stale，不能清空或假称刚刷新。

### REQ-023 · 用户可导入现有 Provider

- **入口与实际行为（未实现）：** 无 import 命令/页面/URI处理；Codex读取只取选中字段，
  不导入 `model_providers` 或 sign-in材料。
- **证据：** S0/SA/SP；没有 Provider 导入专用测试或运行记录。
- **缺口：** 源应用/链接白名单解析、导入预览/确认、冲突、env引用跳过和token交接说明缺失。
- **下一条验收：** 隔离 Codex 定义导入前展示来源/字段/冲突，确认后出现 Provider；源文件
  不变；只有 env引用、无效URL、未知scheme和换行注入均拒绝/跳过并说明。

### REQ-024 · 发现已登录订阅

- **入口与实际行为（未实现）：** 发现 Agent executable 不读取登录；SP没有OAuth账户、
  发现开关、原生CLI生成适配或token刷新。
- **证据：** SA 的 metadata-only契约及E7；无订阅端到端记录。
- **缺口：** 各订阅登录发现/导入/身份/计划/模型/刷新/退出处理、Privacy开关和官方客户端前提。
- **下一条验收：** 使用合成登录材料和受控官方CLI替身验证允许/禁用发现；退出或过期后
  下一请求不服务，不无限刷新。真实账户验证须单独授权和记录。

### REQ-025 · 已弃用订阅的社区插件交接

- **入口与实际行为（未实现）：** 没有插件host、retired订阅库、Move/Adopt/removed状态。
  普通Provider启停不是插件所有权转移。
- **证据：** S0/SP模块与入口；没有本需求运行记录。
- **缺口：** 规格中完整 retired映射、单向迁移、唯一服务者、后台重试、保留登录材料及重装恢复。
- **下一条验收：** 受控旧Cursor账号经Move到合成插件后一次请求仅交插件一次；注入迁移失败
  保留原材料但标记不可服务；停用后停止请求，重装可恢复，不能重新启用退休实现。

### REQ-026 · 套餐、额度和余额

- **入口与实际行为（未实现产品；有输入类型）：** 无 `quota`/余额读取/Usage卡。
  SR有显式Quota/Budget/观察时间类型，仅接受调用者提供的合成快照。
- **证据：** E3 的unknown/stale/耗尽/重置过滤；不含上游额度采集、持久化或预算扣减。
- **缺口：** 真实quota/余额/plan、日周月cap、并发/RPM、客户端key预算、credits策略与历史。
- **下一条验收：** 合成quota源降低账户余额后下一真实请求换到有资格账户；读取失败保留
  上次值和时间、标未知；重置须新观察才能恢复，不能靠时间自动充值。

### REQ-027 · 账户顺序和约束

- **入口与实际行为（未实现产品；有筛选组件）：** SP一条Provider只有一份凭据，
  只能停用整条Provider；SR有账户目标、暂停/权限/预算过滤，未接真实账户源。
- **证据：** E3 的候选排除与E8的Provider级停用；两者不能证明单Provider多账户暂停。
- **缺口：** 默认/专用账户、Agent登录切换、计费seat去重、每账户队列/并发/速率/优先级。
- **下一条验收：** 同一Provider的A/B两账户中暂停A，新增请求仅用B；身份资料不全的座席
  保持独立/未知，不能按相近邮箱合并quota；在途请求处理策略明确。

### REQ-028 · 计划与本机订阅使用限制

- **入口与实际行为（未实现）：** 无 `accounts import`、订阅前提/风险确认/Cloud Project字段。
- **证据：** S0/SP；无账号导入/计划限制专用记录。
- **缺口：** 已文档化导出格式、续期能力校验、席位去重、项目边界和Antigravity风险确认。
- **下一条验收：** 合成双团队席位文件保留两席，重复导入不增生；只有过期access token且无
  续期信息的项拒绝并解释；来源文件和已有账号均不被改写。

### REQ-029 · 账号和模型状态对外说明

- **入口与实际行为（部分）：** Provider列表显示启停、名称/host、模型数、凭据来源；
  CLI公开JSON和Gateway目录不泄密。刷新/配置失败有安全错误，`not_probed`不冒充在线。
- **证据：** E9公开JSON/坏配置拒绝，E2脱敏，E8状态类别与旧模型保留。
- **缺口：** accounts/quotaAPI、当前Agent归属、重登录、迁移/余额/计划、按账户/模型的完整
  故障状态与最近成功时间；许多本地来源故障统一为unavailable。
- **下一条验收：** 模型A被拒而B成功时仅A显示相应故障，账户不被整体停用；登录失效提供
  可达的重登录操作；状态源坏读取保持旧值并显式未知。

## REQ-030–039：Gateway

### REQ-030 · 本地 Gateway 生命周期

- **入口与实际行为（部分）：** `serve`默认绑定127.0.0.1:3425，可选literal loopback端口；
  ProviderStore模式运行真实服务，`/health`与inspect可读；地址/端口失败明确返回错误。
- **证据：** E1/E8真实子进程监听与服务用例；E5历史启动/探测。S0桌面帮助明确要求另起服务。
- **缺口：** GUI默认启动/启停/退出归属、托盘、Web3430、TUI/headless共同所有权、环境监听
  设置、优雅退出/崩溃重启协议与平台证据。UI的Gateway面板目前只是地址输入和查询。
- **下一条验收：** 打开实际桌面后自动就绪并可GET模型；已有同产品外部服务能安全复用且
  退出不误杀它；自有子进程退出时回收；端口被其他进程占用要报告冲突、不改Agent配置。

### REQ-031 · 请求入口与模型标识

- **入口与实际行为（部分）：** SG支持Chat、Responses、Messages、Gemini生成/流式；
  精确provider/model选中上游，升级请求返回426，GET模型目前仅`/v1/models`。
- **证据：** E8的四入口/12对JSON与SSE转换、两Provider路由；token-count测试明确期待501。
- **缺口：** group与裸ID规则、token-count两入口、音频multipart、Codex/Gemini专用目录、
  Ollama目录均未实现。原生协议/推理能力也未由完整catalog声明。
- **下一条验收：** 为已支持协议接通token-count，实际fixture核对模型替换与计数返回；
  然后逐入口验证音频字节/输出格式、目录schema和歧义裸ID，缺能力时明确拒绝。

### REQ-032 · 协议转换和流式行为

- **入口与实际行为（部分）：** SG/SX按客户端与选中Provider协议一次转换；原生路径保留
  原生扩展/流 framing；翻译不支持的字段拒绝，SSE有界并传播取消/不完整结束。
- **证据：** E8 `all_twelve_cross_protocol_pairs_forward_json_once_with_model_and_auth`、
  对应SSE测试、工具参数/大整数/取消/错误测试；E1记录这些测试通过。
- **缺口：** 全部官方字段、reasoning/multimodal/event差异和真实供应商未验收；首token前
  路由状态API缺失；没有显式masking管线。12对合成样例不是全部能力认证。
- **下一条验收：** 固定真实客户端协议版本，补各目标当前缺失的reasoning/图像/工具变体；
  双向验证语义与一次上游调用，未知字段明确兼容错误，取消后连接与缓冲释放。

### REQ-033 · 错误、状态码和请求归属

- **入口与实际行为（部分）：** Store转发使用客户端协议的安全错误体，区分部分HTTP/网络
  类别；选中上游后的响应加Provider/model头；transport counters明确不是Usage。
- **证据：** E8上游401/403/404/429/500、错误正文/Header脱敏、不完整SSE；12对测试检查归属头。
- **缺口：** 无完整稳定request/attempt日志关联、重试最终计数/成本、quota与内容拒绝细分；
  预选中拒绝也没有实际Provider归属可报。转换ResponseContext ID不等于审计trace。
- **下一条验收：** A429/B200的一次请求返回成功、归属B，Route两attempt且Usage最终一次；
  全失败维持客户端错误形态，ID可追踪但日志不含合成秘密或完整正文。

### REQ-034 · 模型目录契约

- **入口与实际行为（部分）：** `/v1/models`、CLI模型命令、GPUIinspect展示启用Provider
  的同源ID/名称；配置健康为not_probed。显式刷新成功才替换持久目录。
- **证据：** E8三Provider旧目录、实时Store目录、16,384模型/转义放大边界及刷新失败保留。
- **缺口：** `/v1/codex/models`、`/v1beta/models`、推理/上下文/native_endpoints公共能力、
  Agent过滤/隐藏但可请求、stale/时间戳缺失；配置文件坏读返回错误而不是完整缓存策略。
- **下一条验收：** 三Provider目录加Codex专用schema可被固定Codex实际读取；目录源离线后
  返回可识别stale的最后有效模型，不能向Agent写入空目录；隐藏项显式请求仍可用。

### REQ-035 · 本机访问和远程鉴权

- **入口与实际行为（部分且模式有限）：** ForwardServer按真实peer限制loopback，要求
  Host与listener匹配，拒绝Origin/Forwarded/X-Forwarded；不开放LAN或管理API。
- **证据：** E8不安全请求零上游；E5本机Host/Origin/proxy防护；非loopback绑定拒绝用例。
- **缺口：** 旧`--catalog`服务没有同等来源校验；无Gateway key、模型/预算限制、Web登录
  cookie、allowlist、TLS监听或可信反代配置。当前拒绝所有Origin不等于已实现Web授权。
- **下一条验收：** 先让三个serve来源模式共享同样的Host/Origin/proxy拒绝；再单独实现显式
  LAN key，验证无key/受限模型在上游计数增加前拒绝，Web cookie不能冒充Gateway key。

### REQ-036 · Agent 连接到网关

- **入口与实际行为（部分）：** Codex事务写Responses base；第三方客户端可手配现有四种
  兼容入口。供应商key只由Gateway添加，不要求本机客户端持有上游key。
- **证据：** E6真实Codex0.165.0经旧候选Gateway拿到合成Responses流并恢复；E8四种客户端fixture。
- **缺口：** 其他Agent原生客户端、Ollama目录配置、远程真实Gateway key以及当前迭代
  原生Codex重跑仍缺。协议fixture不是安装客户端的配置闭环。
- **下一条验收：** 固定Claude/Gemini客户端仅配置本机base和占位认证，实际请求到受控上游；
  客户端不含供应商密钥。以后开启LAN时占位值`magpie`不能获得远程权限。

### REQ-037 · 会话、账户固定和观测 API

- **入口与实际行为（未实现，明确拒绝）：** SG见`X-Magpie-Session`或`X-Magpie-Account`
  返回501；没有账户固定或`/v1/magpie/route`。不从prompt猜会话。
- **证据：** `handle_inner`中的显式拒绝；E5本地guard覆盖历史不支持边界，无正向会话验收。
- **缺口：** 可信原生session规范化、响应回传/CORS、账户选择、route状态及远端继承。
- **下一条验收：** 指定A/B账户的同session请求分别只到指定账户，暂停/无权限账户拒绝且
  不回退；未提供session不造ID；route查询可见真实attempt并不暴露密钥。

### REQ-038 · 远程 magpie

- **入口与实际行为（未实现专用流程）：** 可手工把兼容HTTPS地址当普通自定义Provider，
  但没有Remote Magpie类型、授权目录协商、间隔刷新或session继承。
- **证据：** SP/SC通用URL/目录能力；没有两台Magpie或两个隔离服务的本需求验收记录。
- **缺口：** 远端原生多协议直通、分组/可见范围/版本差异、session归属和远端stale说明。
- **下一条验收：** 两个隔离Magpie只通过公开接口串联，model/session保持且只传对应远端key；
  远端禁用模型/撤key/版本不符时明确失败，保留旧目录并标为非实时。

### REQ-039 · Gateway 的附加查询接口

- **入口与实际行为（未实现）：** `/health`只含传输计数与界限，未实现usage/requests/
  quotas/history/cards/concurrency/limit产品API；未知路径返回未实现。
- **证据：** SG GET分派与`usage_accounting: not_implemented`；无正向查询验收。
- **缺口：** 共享查询数据/时间窗口/筛选/分页，受限key可见范围与稳定授权错误。
- **下一条验收：** 建立真实请求账本后，A客户端key只查询到A的请求；越权quota请求稳定
  拒绝，坏分页/筛选返回4xx且不改变数据，Web登录凭据不能替代Gateway授权。

## REQ-040–049：路由、决策和工具

### REQ-040 · 建立模型路由组

- **入口与实际行为（仅组件）：** SR可构造Group、嵌套展开、启停成员、区分Chat/Decision，
  拒绝空/缺失/循环/重复目标；没有CLI/GUI持久化入口，也未被SG调用。
- **证据：** E3的嵌套/循环/限额/purpose用例；全仓调用关系仅核心和测试使用Router。
- **缺口：** CRUD/复制/改名/排序、自动同名组、隐藏恢复、glob/regex、listed规则、
  `group/id`目录及真正转发。Provider命名空间保留group不等于支持group请求。
- **下一条验收：** CLI保存组并重启服务，GPUI与目录看到同一`group/work`；Agent请求真正
  到成员；空组/环/缺失引用拒绝且保留旧store；decision组不能漏进聊天目录。

### REQ-041 · 路由策略

- **入口与实际行为（仅组件）：** SR实现smart/order/rotate/usage/pace/weight/fast与fast别名，
  状态仅在Router实例中。Free first/Manual没有枚举或代码路径，九种策略未齐备。
- **证据：** E3的36项纯测试含3:1平滑权重、无数据回退、重置/速度比较；无上游attempt。
- **缺口：** 全策略用户设置/持久化与Gateway接线，多key组合、stays、受限成员位置、首响应
  等待、再次尝试窗口/上下文/推理规则；order选第一个不等于429后fallback。
- **下一条验收：** 同一常驻Router处理真实4n个请求，3:1平滑且不因每次重建退回首项；
  order的A429后B200有真实两次attempt。Manual失败不切成员、Free first依明确定义另验。

### REQ-042 · 选择可服务成员和 account 顺序

- **入口与实际行为（仅组件）：** SR对显式快照筛选暂停/权限/预算/未知或过期quota，
  优先非plan-limited；soft rate sink与resting需明确策略。SG仅按启用Provider精确选模型。
- **证据：** E3的unknown/stale/quota/reset/plan限制/sink/resting测试；不含真实账户采集。
- **缺口：** 账户刷新源、原子预算/并发预留、Provider fallback model、实际429回填和调度层级。
- **下一条验收：** 两账户仅B拥有目标模型权限时真实请求只到B；A注销或quota陈旧后不得
  继续服务；API/local明确无quota数据，不得伪造无限余额来满足Router输入。

### REQ-043 · 按请求上下文匹配路由规则

- **入口与实际行为（未实现）：** 无规则store/编辑器/classifier；SR的Context不读取prompt
  意图或执行分类请求，SG也未调用路由引擎。
- **证据：** SR自身边界说明与S0分派；无本需求运行记录。
- **缺口：** Agent/session/project/effort/预算/时间规则、优先级、用户同意、有效label校验和解释。
- **下一条验收：** 明确开启的合成bug classifier选定成员并记录命中；关闭时零分类请求；
  非法label/超时只走一次规定fallback或明确错误，不把输出作为任意model/URL执行。

### REQ-044 · Session affinity 和重置

- **入口与实际行为（未实现）：** SG拒绝session header；SR没有会话粘性存储。
  rotate/weight调度状态不是session affinity。
- **证据：** SG显式501及SR接口/边界；无双轮会话正向测试。
- **缺口：** auto/session/turn/off/strict、原生ID识别、远端继承、缓存恢复/重置和会话历史。
- **下一条验收：** 同session两轮在可用时保持同一账户；换成员顺序不漂移；strict耗尽明确
  失败而非换账户；无session不根据正文或IP推断；损坏粘性缓存可识别并按默认策略处理。

### REQ-045 · 重试和 Fallback 边界

- **入口与实际行为（未实现）：** SG每请求一个上游attempt且禁HTTP隐式retry；SR只选择。
  轮换竞争时重读凭据一次不是上游重试，更不是fallback链。
- **证据：** E8一次请求只发一次与取消测试；E3明确排除传输retry的范围。
- **缺口：** 分类、次数/时间/退避、请求重放安全、成员去重、取消后停止、tier降级和最终
  Usage去重；限流/内容拒绝不应混为quota。
- **下一条验收：** A429/B200请求A/B各一次且最终成功一次；用户在A后取消则B零请求；
  全部内容拒绝不长期休眠账号；有副作用/已输出流的请求按明确可重放边界拒绝重试。

### REQ-046 · 实时路由解释和历史

- **入口与实际行为（仅组件）：** SR的Decision/Exclusion可解释一次纯选择；SG health有
  聚合transport counters。没有Routing页面、事件流或请求/session历史。
- **证据：** E3的选择/排除原因；SG显式区分传输完成与客户端交付/Usage。
- **缺口：** 实时attempt状态、ID关联、历史持久化、筛选/成本/名称/隐藏email与坏日志提示。
- **下一条验收：** A受限、B回答时可观察A rate-limited→B answering→answered，按同一
  request/session查两attempt；日志损坏显示不完整，不渲染为“没有请求”。

### REQ-047 · System One / 模型决策

- **入口与实际行为（未实现产品；有purpose隔离）：** SR的Decision组不会混入Chat展开；
  没有System One Provider/endpoint/classifier调用或用户开关。
- **证据：** E3 `decision_groups_require_explicit_purpose_and_cannot_leak_into_chat_expansion`；
  无正常decision请求记录。
- **缺口：** `/v1/systemone`等效接口、choice schema、独立额度/尝试、最少数据发送与fallback。
- **下一条验收：** 输入固定choices只接受其中之一；坏JSON/未知choice/超时不执行任意URL，
  使用规定fallback且路由可解释；关闭自动决策后上游分类调用数保持零。

### REQ-048 · Web 搜索配置和 Provider 代搜

- **入口与实际行为（未实现）：** 无searcher配置、CLI search、搜索代理或多key调度。
  协议中的普通tool消息转换不代表执行搜索服务。
- **证据：** S0/SX实际分派与模块边界；无搜索端到端记录。
- **缺口：** 规格中的搜索提供者、顺序/开关/轮换/base、无结果fallback、quota和错误保留。
- **下一条验收：** 受控搜索A无结果、B成功时只按顺序尝试并返回B结果；401不删除key；
  关闭某服务后零请求，全部失败提供明确错误而非成功空内容。

### REQ-049 · Agent 工具服务

- **入口与实际行为（未实现）：** 无MCP server、`tools`/`tool`命令、web_fetch或媒体生成。
  SX只转换客户端已提供的tool schema/调用/结果，不会执行它们。
- **证据：** S0/SX；工具参数保真测试仅证明转译，不证明MCP/媒体行为。
- **缺口：** 能力清单、搜索/读取/视觉/图像/视频、参数/URL/输出路径验证、取消与临时文件清理。
- **下一条验收：** Agent经受控MCP调用web_search得到标题/URL/摘要；媒体成功才原子写出
  指定文件；超时/不支持能力/路径逃逸均失败并清理未完成文件，不误改已有文件。

## REQ-090–095：安全、隐私和平台

### REQ-090 · 凭据存放和界面遮蔽

- **入口与实际行为（部分）：** SP直接key输入默认遮蔽，系统keyring保存，公开配置仅引用；
  CLI stdin不经ARGV，公开列表不读/打印key；锁定/缺失凭据阻止认证，轮换影响新请求。
- **证据：** E2真实Linux Secret Service；E9失败回滚/不确定提交/清理待处理/公开脱敏；
  E1输入组件测试与原生凭据布局Scene。
- **缺口：** 已存key尾码、显式copy/reveal未实现；OAuth/Gateway/Web/同步凭据和加密备份
  不存在。macOS/Windows当前运行、原生读屏/剪贴板未验收；Android直接key存储不可用。
- **下一条验收：** 在真实窗口输入/替换合成key，列表只显示规定尾码且复制需明确操作；
  锁定keyring时认证零上游，旧引用保留；再分别运行各系统后端失败/恢复用例。

### REQ-091 · 网络入口、鉴权和限流

- **入口与实际行为（部分；旧表漏记）：** 默认ForwardServer具备peer/Host/Origin/proxy
  保护与连接、请求、body、目录并发上限；loopback无客户端key。
- **证据：** E8 unsafe零上游与容量边界，E5历史本机保护/并发取消；没有远程授权测试。
- **缺口：** `--catalog`未共享来源防护；没有LAN授权key、撤销、白名单/cap/RPM、Web cookie/
  CSRF/CORS/可信代理的一致事实源或拒绝日志request ID。资源上限不是按账户限速。
- **下一条验收：** 先在全部服务模式验证伪Host/Origin:null/代理头拒绝；显式LAN模式再验证
  A key不能请求B模型，撤销后下一请求拒绝且上游计数不变，正常本机请求仍可用。

### REQ-092 · Privacy 默认和本机数据

- **入口与实际行为（基础默认/保护部分）：** 目前没有prompt archive或正文日志产品；
  SG输出安全错误，显式Codex变更提供路径/预览/恢复。桌面设置只有现有外观/语言等实现。
- **证据：** E8合成秘密不进入错误体/响应头；E4预览不写与恢复映像私有权限。没有覆盖
  “所有运行目录/生产日志不存在正文标记”的完整Privacy专项证据。
- **缺口：** Privacy录制开关、位置/保留期/容量/同步范围/删除、session与正文分离、完整日志
  审计未实现。没有录制功能不能当作开启/关闭/清理整条验收通过。
- **下一条验收：** 新安装发送独特正文标记，检查仅自有运行目录/日志无正文；实现显式录制后
  验证设置写失败不能显示成功，关闭后不再新增正文，清理保留合法元数据。

### REQ-093 · Private URL、代理和 SSRF

- **入口与实际行为（部分）：** SP/SC仅允许HTTPS或明确loopback HTTP，校验URL结构；
  localhost固定为loopback、拒self-target；transport证书校验、无redirect/隐式proxy/retry，响应有界。
- **证据：** E8刷新redirect零目的请求和大小边界；SC的URL/认证header测试及E9 URL拒绝。
  超时配置可在transport/refresh源码核对，不把配置存在当作耗满期限的专项验收。
- **缺口：** 没有显式代理/Agent代理、场景化allowlist、私有Git/npm/MCP/search下载器的DNS/
  跳转规则；用户配置Provider的URL策略不能冒充所有不可信下载场景的SSRF完成。
- **下一条验收：** 在受控DNS/redirect fixture中分别验证“用户明确私有上游”可用与“不可信
  页面跳本机admin”被拦；显式proxy凭据不输出，协议/解析/取消边界均有零越界请求断言。

### REQ-094 · 多平台和 WSL 边界

- **入口与实际行为（部分）：** 本轮Linux x86_64构建/CLI/Gateway/Secret Service及GPUI离屏
  Scene有证据；Unix配置事务有平台实现，Windows变更明确拒绝，GPUI无Android后端。
- **证据：** E1及[包装说明](../packaging/README.md)；早前Android/跨平台结果按各自旧版本保留。
- **缺口：** 当前macOS/Windows/Android/NixOS/容器/WSL运行与安装恢复未重验；新增libdbus
  使旧musl构建结论不能直接沿用。无WSL distro发现、睡眠状态或隔离写入。
- **下一条验收：** Windows/WSL两个隔离配置各自连接，不向WSL复制本机token；列举停止distro
  不启动；然后按当前源码逐平台构建/执行，Android环境引用和原生GUI能力分别标记。

### REQ-095 · 隐私工具与外部上游

- **入口与实际行为（基础部分）：** SC只读取选中Provider的凭据；SG构造上游认证，不沿用
  客户端Cookie/Authorization，不遍历Library/其它Agent文件；只有明确模型刷新另行出网。
- **证据：** E8两Provider认证/一次请求、E5客户端认证与Cookie不泄漏，E2选中key；
  本条工具正常验收尚无证据。
- **缺口：** 自动分类/search/media工具不存在，故缺账户/模型说明、Usage成本、受限key权限
  与禁用后零隐式调用的完整链路。
- **下一条验收：** 实现首个工具后显示所用Provider/模型并计入实际Usage；关闭search/tool
  后其合成上游调用数不再增长；与本次请求无关的合成密钥/指令标记不能出现在上游记录。

## 下一轮建议顺序和验收门槛

| 顺序 | 具体用户问题 | 最小可交付行为及不可省略边界 |
| --- | --- | --- |
| 1 | 打开桌面仍无法直接用已保存Provider | Gateway生命周期：启动/就绪/故障/退出，区分自己拥有与外部服务，不误杀、不伪报端口健康；所有服务模式复用来源检查 |
| 2 | Codex接入后每次换模型必须先断开 | 已托管模型更新：保留第一次恢复点，最后写入映像跟进新模型，CAS与外部目标字段冲突拒绝；断开恢复首次用户状态 |
| 3 | 常见厂商也要手工填协议和URL | 共享Provider预设/地区目录，CLI和GUI使用同一数据；先让DeepSeek仅key、本机Ollama无key形成真实闭环；未支持订阅/插件类型明确标界 |
| 4 | 已有七种路由算法无法被用户使用 | 版本化组store/CLI/GUI→目录group/id→常驻Router→真实上游；配置修订刷新不能每请求重置rotate/weight，unknown quota不得伪造 |
| 5 | Fallback与Usage缺少共同账本 | 先定义稳定request/attempt归属，再接有界可重放retry；A429/B200只计最终一次成功，取消不再尝试，敏感正文不落日志 |

上述顺序是根据当前断点提出的实施顺序，不缩减46条或完整93条合同。
每次只把已经执行并有来源的具体子流程写回覆盖表；入口未接、真实平台未跑、原版
差分未做、用户尚未认可的部分仍分别保留。

# Provider 工作流与原版 UI 对齐记录

日期：2026-10-30。软件版本：0.2.0 开发快照。实现基线：
`f20439bcc03d527f8c713b1b22c066604e16ba7a`。本记录对应所在提交的实现；
逐文件指纹、命令与结果见
[`tests/results/provider-ui-parity-2026-10-30.json`](../tests/results/provider-ui-parity-2026-10-30.json)。

## 本轮解决的问题

原有 CLI、桌面与 Gateway 已有 Codex 接入和协议转换组件，但缺少用户能够实际管理的
Provider 数据源。本轮把“添加供应商 → 保存凭据 → 获取模型 → 选择模型 → 通过网关请求”
接成同一条工作流，并按原版 Magpie 的列表层级、颜色和导航规则调整 GPUI。

桌面、CLI 与运行中的 Gateway 共享 `providers.toml`。桌面直接使用 core 中的配置及凭据事务；
模型刷新交给匹配的 CLI 执行。Gateway 在每个新请求上读取配置，按完整的
`provider/model` 选择启用的 Provider，读取其密钥，再进入既有协议转换及流式处理。
停用保留配置，删除移除配置，轮换密钥影响后续请求；在途请求继续使用已经选中的上游。

这是完整 93 条需求中的一次实质实现增量。完整需求仍由父仓库
[`COVERAGE.md`](https://github.com/LIghtJUNction/magpie-rs/blob/main/recipes/magpie/COVERAGE.md)
追踪；下面的组件证据不等于全部产品、平台和原版行为已经通过验收。

## 对照来源与设计决定

原版为 [yetone/magpie](https://github.com/yetone/magpie)，本轮固定读取提交
[`dca3e2d716fe21bbb3e239f769a8e7861b135c01`](https://github.com/yetone/magpie/tree/dca3e2d716fe21bbb3e239f769a8e7861b135c01)。
参考内容包括 `internal/gui/assets/index.html`、`app.css`、
[公开产品界面](https://usemagpie.ai/)及[入门工作流](https://usemagpie.ai/docs/start)。
父仓库的 `ORIGINAL_PARITY.md`、`ORIGINAL_WEB_BEHAVIOR.md` 和 `UI_DIRECTION.md`
补充此前固定版本的隔离操作观察与验收要求。静态页面、原版运行观察与本次 Rust 测试分开记录。

| 原版观察或需求 | 本轮实现 | 仍保留的差异 |
| --- | --- | --- |
| Agent 以连续行显示身份、模型、状态和操作 | 保留可用 Agent 列表及 Codex 行内模型入口，详情按需展开 | 其他 Agent 仍主要是发现/只读展示 |
| Provider 行优先显示名称、host、模型数和状态 | 新增原生 Provider 列表、搜索、编辑与启停 | 原版预设厂商、使用中的 Agent 标记和账户详情未齐备 |
| 停用项保留资料并单独呈现 | 停用配置持久化，目录及新请求排除停用项 | 不等于多账户暂停及调度 |
| 自定义 Provider 可直接输入 API key | 原生遮蔽输入与系统凭据存储；环境变量是显式可选来源 | 每 Provider 当前一份凭据，未实现多 key 排序/预算/历史归属 |
| 刷新模型是独立操作，失败保留旧目录 | 实际请求并完整验证后提交，仅更新 models 字段 | 不含公共价格/能力补全、过滤规则和周期刷新 |
| 浅色白灰、深色炭黑、小面积紫色强调 | 统一背景、卡片、文字、边框与选中态 token | 全产品页面、用户最终视觉认可仍待验收 |
| 宽窗侧边导航，中窄窗保留主任务 | 宽窗侧栏、中窗顶部、360/440 px 重排与可滚动表单 | 托盘快捷 Panel、Web/TUI 仍未实现 |
| 五语与持久化偏好 | 英文、简中、繁中、日文、德文及主题保存 | 原生 IME/读屏、完整日期数字本地化未验收 |

当前导航只开放已经有实际行为的 Agents、Providers、Settings；其余功能在路线图中说明。
原版完整导航和操作目标继续保留在配方中。预览使用明确标记的合成 Provider 与模型，
生产界面从本地真实状态读取数据。

## 配置、凭据和模型刷新

### 公共配置

`crates/magpie-core/src/provider_store.rs` 定义版本化 schema、校验和 revision 检查。
公开字段包含稳定 ID、显示名称、base URL、协议、启用状态、模型和凭据引用；文件不保存 API key。
缺失文件按空配置处理；损坏或不支持的文件明确拒绝读取，不用空配置覆盖原内容。

写入经过同一配置锁、版本检查和私有临时文件替换。Linux/macOS/Android 路径使用
目录文件描述符及禁止跟随链接的打开方式，保留原子替换和同步边界；Windows 写入明确返回暂不支持。
并发修改时拒绝旧草稿，用户需要重读后处理冲突。若替换后的同步失败，结果说明提交可能已经发生，
要求重读，不能声称原配置一定未变。
不遵守同一锁的外部写入仍可能在最后一次检查与 rename 之间竞争，文件系统替换本身不是硬件级 compare-and-swap。

| 边界 | 当前值与行为 |
| --- | --- |
| Provider store | 最大 1 MiB、128 个 Provider |
| 模型数量 | 每 Provider 4,096，全部合计 16,384；超限拒绝，完整列表不截断 |
| URL | 远程 HTTPS；HTTP 仅 loopback；拒绝 URL 中的用户信息、query、fragment 及错误操作路径 |
| 密钥输入 | 1–8,192 个可见 ASCII 字符，不接受空白/控制字符 |
| 刷新响应 | 最多 8 页、每页 1 MiB、合计 4 MiB；网络期限 15 秒 |
| Gateway 模型目录 | 最大 32 MiB，最多两个目录响应同时发送，按 16 KiB 分块 |

目录 JSON 的大小与 store 文件大小不同：Provider 名称和 ID 会出现在每个模型行中，
JSON 转义也会放大内容。边界 fixture 使用 4 × 4,096 个模型、128 字符 Provider ID、
256 字节含引号/反斜杠名称和 50 字符模型 ID；合法 store 的完整输出是 17,268,762 字节。
测试检查完整 16,384 行与名称，避免用较小输出限制误拒绝有效数据。

### 系统凭据

`credentials.rs` 使用 keyring 的本机后端。Linux 使用 Secret Service，macOS 使用 Keychain，
Windows 对接系统凭据后端；本轮实际执行了 Linux Secret Service 验证，另两平台尚未运行。
公共配置只保存随机、不包含密钥内容的引用。CLI 密钥由 stdin 提供，桌面通过遮蔽输入提供，
都不接受密钥作为 CLI 参数，也不向日志和公共 JSON 输出密钥。

轮换先保存一个新引用，再以原 revision 提交公开配置，成功后清理旧引用。配置提交前失败时
尝试回收新引用；提交状态不确定时保留凭据供重读恢复。公开配置已经提交但旧 key 清理失败时，
明确返回 `credential_cleanup_pending`，避免将已提交配置误报为没有保存。

没有可用 Secret Service 时直接输入密钥的保存会报错，保留旧配置。用户可以主动选择
`--key-env NAME`，也可以为不需要认证的本地服务保存无密钥配置。已经引用却无法读取的密钥
会阻止上游请求，不会自动变成无认证请求。

### 模型发现和请求

每个 Provider 选择 OpenAI Chat、OpenAI Responses、Anthropic Messages 或 Gemini 中的一种协议。
模型刷新复用 Gateway 的 HTTP transport 和认证规则：校验证书、拒绝重定向、禁用隐式环境代理，
只向用户选择的 Provider 发送其凭据。Anthropic 和 Gemini 使用对应分页游标，Gemini 将资源名
转换为模型 ID，并在明确能力字段存在时过滤不支持 `generateContent` 的项目。

只有完整且有效的目录可以在原 revision 上提交。401、坏 JSON、空列表、未知/不完整分页、
重复游标、容量超限和并发修改都有保留上次目录的测试。刷新进程被取消或回复丢失时，
界面重新读取当前 store 并说明结果未确认，因为持久化可能早于取消完成。

Gateway 按请求加载共享 store 与选中凭据，不需要为编辑/启停/轮换重启进程。
同名模型由 Provider 前缀区分。配置的 Provider 状态保持 `not_probed`，
模型出现在列表中仅证明它已配置。现有四协议 JSON/SSE 转换继续拒绝无法可靠表示的字段，
不将有限合成用例解释为所有供应商接口的完整兼容。

## 验证方法与证据

本轮在 Linux x86_64 上使用 Rust 1.99.1，构建缓存位于仓库之外。
workspace 测试在无显示环境运行，所有网络用例使用隔离的 loopback 合成上游。
桌面是独立 GPUI workspace，使用固定依赖 revision 与实际 GPUI Scene 渲染。
测试数量、命令、源码指纹及渲染 manifest 摘要保存在本目录所链接的机器结果中。

| 检查 | 证明的行为 |
| --- | --- |
| Workspace 全 targets：298 passed，4 个辅助进程入口按测试设计忽略 | 既有事务/协议回归，以及新增 store、CLI、刷新、实时 Provider 转发和目录边界 |
| Workspace Clippy `-D warnings`、fmt | 本次根 workspace 的静态检查及格式 |
| 真实 Secret Service 工作流：7 项通过 | 跨进程读 key、输出脱敏、模型刷新、轮换、停用、移除清理、Codex 预览/保存/精确恢复 |
| 独立桌面：68 passed、1 个既有单独执行入口忽略；全目标 Clippy 与 fmt 通过 | 输入/偏好/状态、CLI bridge、关闭/取消等具体代码路径 |
| 原生 GPUI：中文 48 场景、德语窄窗 12 场景，均保留 1x/2x PNG | 实际布局、文本、图标与控件的像素输出；不包含真实账户 |
| 现有打包自检 13 项、shell 语法、实际 ELF 依赖检查 | Linux CI/Arch/AUR 补齐 D-Bus，Deb 自动依赖能识别本地二进制的 libdbus |

Secret Service 用例创建独立 D-Bus 会话及私有 XDG 目录，启动独立 GNOME Keyring，
向本地 HTTP fixture 发送合成 key。保存与轮换分别由 CLI 进程执行，Gateway 是另一个进程。
测试核对上游真正收到的 key、模型 ID、停用后的零上游请求，并要求旧凭据清理状态为成功。
结束后停止自有服务并删除临时数据。

Codex 检查使用合成 TOML：dry-run 不写，apply 保留自定义字段，disconnect 恢复原字节。
本轮没有重新执行已安装 Codex 客户端的真实推理用例；此前的这类记录仍保留为历史证据。

### 复现

```sh
CARGO_TARGET_DIR=/tmp/magpie-core-check cargo test --workspace --all-targets --locked
CARGO_TARGET_DIR=/tmp/magpie-core-check cargo clippy --workspace --all-targets --locked -- -D warnings
cargo fmt --all --check
CARGO_TARGET_DIR=/tmp/magpie-core-check cargo build --locked -p magpie-cli
python3 -B scripts/verify-provider-keyring.py --magpie /tmp/magpie-core-check/debug/magpie

CARGO_TARGET_DIR=/tmp/magpie-desktop-check cargo test --manifest-path apps/desktop/Cargo.toml --locked --features visual-test --lib
CARGO_TARGET_DIR=/tmp/magpie-desktop-check cargo clippy --manifest-path apps/desktop/Cargo.toml --locked --all-targets --features visual-test -- -D warnings
CARGO_TARGET_DIR=/tmp/magpie-desktop-check cargo build --manifest-path apps/desktop/Cargo.toml --locked --features visual-test --example render_preview
WGPU_BACKEND=vulkan LIBGL_ALWAYS_SOFTWARE=1 /tmp/magpie-desktop-check/debug/examples/render_preview --output /tmp/magpie-native-preview --all --locale zh-CN
```

Linux 构建需要相应 GPUI 图形依赖；Secret Service 检查还需要 `dbus-run-session` 和
`gnome-keyring-daemon`。原生渲染需要可用的图形后端或软件渲染环境。
CLI 测试与日常 headless Gateway 不启动 GPUI。

## 尚未覆盖的原版范围

Provider 预设目录、OAuth/订阅、多账户与多 key 调度、费用/能力元数据、自动模型过滤、
高级端点/代理/限流配置与低成本请求测试仍未完成。已经完成纯计算的路由策略尚未接入
持久化和实际 Gateway；Usage、Sessions、Masking、Plugins、Library、同步和 Web/TUI
也仍有完整需求等待实现。

桌面尚不负责 Gateway 的启动/停止与托盘生命周期。除 Codex 外的 Agent 配置写入、
完整 Profile 和逐 Agent 模型可见性仍待完成。外部真实上游账户、macOS/Windows/Android
当前变更、原生窗口键盘/IME/读屏及用户视觉认可均需各自的验收证据。

普通窗口关闭会等待后台保存任务完成；GPUI 当前版本的直接应用退出只有约 200 ms 清理预算，
强制终止进程也无法保证等待所有工作。离屏凭据 Scene 使用测试专用初始滚动位置，
因为纹理捕获不推进真实窗口的 display-link 焦点回调。底层存储或系统凭据失败时，
五语操作提示后附加的安全技术详情目前仍可能是 core 的英文错误。

本轮没有发布安装包、替换用户安装或重测正式 release 性能；早期体积、延迟和设备结果
继续作为其对应版本的历史记录。后续优先接通桌面 Gateway 生命周期及路由组，
再用实际请求归属支撑 Usage 和账户能力，避免界面先显示没有数据来源的状态。

# Provider API preset plugins

核实日期：**2026-11-02**。21 个预设，覆盖 **20 家不同服务/本地引擎**；百炼北京和新加坡是同一家服务的区域选项，不计为两家。Qwen 是百炼提供的模型系列，不再另算一个供应商。

本次只核对官方一手文档与现有源码的 URL/认证/协议组合，**没有使用真实账号或密钥，没有对这些服务发起推理或账号验收**。文档示例中的模型不表示用户账户已开通；预设不包含模型、价格、额度或在线状态。初始模型列表应为空，用户刷新或手动输入后才有可选模型。

## API 与网关约定

供应商数据仅位于 `plugins/providers/<brand>/plugin.toml`，每品牌一个运行时插件包。core 的 `ProviderPreset` 是拥有所有权的六字段通用类型，以及复用 `Provider::validate` 的通用 `validate()`；没有厂商列表、`PRESETS` 或静态 `find`。Registry 负责发现、注册、按插件启停、冲突处理和查询；调用方不能回退到 core 静态厂商数据。`ProviderPreset` 字段为 `id/name/base_url/source: String`、`protocol: ProviderProtocol`、`requires_key: bool`。预设 ID 是模板标识，多个同厂商 Provider 应有独立实例 ID。

统一 manifest schema（单项和多区域使用相同数组结构）：

```toml
format = "magpie-plugin/1"
id = "provider.example"
name = "Example API"
version = "0.1.0"
api_version = 1
kind = "provider-preset"
enabled_by_default = true
requires = []
provides = ["provider-preset"]

[[contributions.providers]]
id = "example-region"
name = "Example API (Region)"
base_url = "https://api.example.test/v1"
protocol = "openai_chat"
requires_key = true
source = "https://docs.example.test/api"
```

`api_version = 1` 是插件宿主接口版本；`requires = []` 表示数据预设不依赖其他插件，`provides = ["provider-preset"]` 声明贡献能力。顶层 `id` 是插件启停单位，`contributions.providers[].id` 是全 Registry 内唯一的预设标识。百炼包有两个完整贡献项；不使用 `[provider]` 加区域覆盖的混合形式。关闭一个品牌插件应撤下该插件全部预设；不暗示删除已保存 Provider、凭据或 Usage 历史。运行时发现、打包携带与 UI 接线由 Registry/主代理完成，本数据任务不将其标为已经验收。

调用方把所选预设复制到可编辑表单，收集凭据后再通过共享 ProviderStore 保存；预设本身不能代表保存、已连接或测试通过。`requires_key` 是表单提示，不代替服务认证与凭据校验；`false` 只表示本机无认证默认配置可不填 key，仍允许用户输入本地服务的 token。

网关现有四种协议及 `forward/source.rs` 实际拼接方式：

| 协议 | base 后追加的请求路径 | 认证 |
| --- | --- | --- |
| `openai_chat` | `/chat/completions` | `Authorization: Bearer <key>`；本地无 key 时不发认证头 |
| `openai_responses` | `/responses` | 同上 |
| `anthropic_messages` | `/messages` | `x-api-key` 和固定 `anthropic-version: 2023-06-01` |
| `gemini` | `/models/{model}:generateContent` 或 `:streamGenerateContent?alt=sse` | `x-goog-api-key` |

下表中的 base 已含版本路径；不能再追加一层 `/v1`。特别保留 Fireworks 的 `/inference/v1`、Groq 的 `/openai/v1`、智谱的 `/api/paas/v4`、百炼的 `/compatible-mode/v1`。本地文档示例的 `localhost` 使用等价的 `127.0.0.1`，明确绑定 loopback，不依赖 DNS。

默认协议仅表示本次选定的 API 模式。供应商可能还提供其他协议，但不能从 Chat 支持推断 Responses/Messages/Gemini 全部可用。协议兼容也不表示所有工具、reasoning、图片、语音、搜索、服务端状态或专有字段都可由现有网关转换。SDK 示例要求的虚构 key 不应保存为真实凭据。

## 逐项证据

每行日期均为实际文档核实日期；“Bearer”指 API key，不是 OAuth 或订阅 token。

| ID / 名称 | API base URL | 默认协议 / 认证 | 官方来源 | 日期 | 支持范围与限制 |
| --- | --- | --- | --- | --- | --- |
| `openai` · OpenAI | `https://api.openai.com/v1` | Responses / Bearer | [官方文档](https://developers.openai.com/api/reference/overview) | 2026-11-02 | Responses；API key，不是 ChatGPT 订阅。组织/项目专属 Header 不在此预设中。 |
| `anthropic` · Anthropic | `https://api.anthropic.com/v1` | Messages / x-api-key | [官方文档](https://platform.claude.com/docs/en/api/overview) | 2026-11-02 | Messages API key；网关固定 anthropic-version: 2023-06-01，不含 Claude Code 订阅、beta Header。 |
| `gemini` · Google Gemini (AI Studio) | `https://generativelanguage.googleapis.com/v1beta` | Gemini / x-goog-api-key | [官方文档](https://ai.google.dev/api) | 2026-11-02 | GenerateContent / streamGenerateContent；不是 Interactions、Vertex AI 或 OAuth。 |
| `openrouter` · OpenRouter | `https://openrouter.ai/api/v1` | Chat / Bearer | [官方文档](https://openrouter.ai/docs/api_reference/overview) | 2026-11-02 | Chat；模型 ID 保留厂商前缀。不配置可选 HTTP-Referer/X-Title，不含供应商路由扩展。 |
| `deepseek` · DeepSeek | `https://api.deepseek.com/v1` | Chat / Bearer | [官方文档](https://api-docs.deepseek.com/quick_start/agent_integrations/workbuddy/) | 2026-11-02 | Chat；官方 WorkBuddy 接入示例明确使用 /v1/chat/completions；不硬编码推理模式或特定模型。 |
| `siliconflow-cn` · SiliconFlow (China) | `https://api.siliconflow.cn/v1` | Chat / Bearer | [官方文档](https://docs.siliconflow.cn/docs/api/chat-completions-post) | 2026-11-02 | 中国站 API key 和模型；不能将中国站与国际站账户混用。 |
| `bailian-cn` · Alibaba Bailian / Qwen (Beijing, legacy host) | `https://dashscope.aliyuncs.com/compatible-mode/v1` | Chat / Bearer | [官方文档](https://help.aliyun.com/zh/model-studio/compatibility-of-openai-with-dashscope) | 2026-11-02 | 北京旧公共域名，官方明确仍可用；建议编辑为本账户工作空间专属域名。只支持按量 API，非 Coding/Token Plan。 |
| `bailian-sg` · Alibaba Model Studio / Qwen (Singapore, legacy host) | `https://dashscope-intl.aliyuncs.com/compatible-mode/v1` | Chat / Bearer | [官方文档](https://help.aliyun.com/zh/model-studio/compatibility-of-openai-with-dashscope) | 2026-11-02 | 新加坡旧公共域名，官方明确仍可用；使用对应地区 key。建议改为工作空间域名，不代表美国/香港地区。 |
| `groq` · Groq | `https://api.groq.com/openai/v1` | Chat / Bearer | [官方文档](https://console.groq.com/docs/openai) | 2026-11-02 | Chat 兼容子集；供应商不支持的 OpenAI 参数仍会报错。 |
| `together` · Together AI | `https://api.together.ai/v1` | Chat / Bearer | [官方文档](https://docs.together.ai/docs/inference/openai-compatibility) | 2026-11-02 | 默认服务端点；不表示美国专属地区端点、专用部署或所有模型权限。 |
| `fireworks` · Fireworks AI | `https://api.fireworks.ai/inference/v1` | Chat / Bearer | [官方文档](https://docs.fireworks.ai/api-reference/post-chatcompletions) | 2026-11-02 | 推理 Chat 路径，保留 /inference；不是管理 API。不预设专用部署模型。 |
| `mistral` · Mistral AI | `https://api.mistral.ai/v1` | Chat / Bearer | [官方文档](https://docs.mistral.ai/api) | 2026-11-02 | Chat；不含 La Plateforme Agents、FIM、OCR 或专用 Codestral 端点。 |
| `xai` · xAI | `https://api.x.ai/v1` | Responses / Bearer | [官方文档](https://docs.x.ai/developers/rest-api-reference/inference/responses) | 2026-11-02 | Responses；仅现有网关可表达的请求，不承诺托管搜索工具等服务端扩展。 |
| `zhipu-cn` · Zhipu GLM (China) | `https://open.bigmodel.cn/api/paas/v4` | Chat / Bearer | [官方文档](https://docs.bigmodel.cn/api-reference/模型-api/对话补全) | 2026-11-02 | 中国智谱按量 API；保留 /api/paas/v4，不添加 /v1。不是 Coding Plan 或国际 Z.ai。 |
| `moonshot-global` · Moonshot / Kimi (Global) | `https://api.moonshot.ai/v1` | Chat / Bearer | [官方文档](https://platform.kimi.ai/docs/api/chat) | 2026-11-02 | 全球平台 API key；非中国站 .cn、Kimi Code 或消费者订阅。 |
| `minimax-global` · MiniMax (Global) | `https://api.minimax.io/v1` | Chat / Bearer | [官方文档](https://platform.minimax.io/docs/api-reference/text-openai-api) | 2026-11-02 | 全球 OpenAI 兼容文本 API；不是中国 .com 账户、M Plan、音频或视频 API。 |
| `cerebras` · Cerebras | `https://api.cerebras.ai/v1` | Chat / Bearer | [官方文档](https://inference-docs.cerebras.ai/quickstart) | 2026-11-02 | Chat；账号可用模型、上下文和速率以服务为准。 |
| `nvidia` · NVIDIA NIM (Hosted API) | `https://integrate.api.nvidia.com/v1` | Chat / Bearer | [官方文档](https://docs.api.nvidia.com/nim/reference/llm-apis) | 2026-11-02 | NVIDIA 托管 Chat API；不是本地 NIM 容器或所有目录模型的可用性承诺。 |
| `ollama` · Ollama (Local) | `http://127.0.0.1:11434/v1` | Chat / 默认无需 key，可选 Bearer | [官方文档](https://docs.ollama.com/api/openai-compatibility) | 2026-11-02 | 本机 OpenAI 兼容 Chat，无需 key；需先启动服务并拉取模型。不是 Ollama Cloud 或原生 /api/chat。 |
| `lmstudio` · LM Studio (Local) | `http://127.0.0.1:1234/v1` | Chat / 默认无需 key，可选 Bearer | [官方文档](https://lmstudio.ai/docs/developer/openai-compat) | 2026-11-02 | 示例端口 1234；需启动服务。有令牌认证时必须另填 Bearer token，不应以可选密钥绕过服务认证。 |
| `vllm` · vLLM (Local) | `http://127.0.0.1:8000/v1` | Chat / 默认无需 key，可选 Bearer | [官方文档](https://docs.vllm.ai/en/latest/serving/online_serving/openai_compatible_server/) | 2026-11-02 | 默认端口 8000；需运行适用 Chat 模板的模型。若设置 --api-key/VLLM_API_KEY，另填对应 Bearer key。 |

## 区域、认证与目录边界

- 百炼官方中文文档明确旧域名仍可使用，并建议迁移到工作空间专属域名。北京推荐 `https://{WorkspaceId}.cn-beijing.maas.aliyuncs.com/compatible-mode/v1`，新加坡推荐 `https://{WorkspaceId}.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1`；花括号是说明模板，**不能作为默认配置保存**。用户应根据控制台的真实工作空间和地区修改 base。各地区 key 不互换。英文文档的北京表格与正文迁移域名存在不一致，本次采用中文文档明确的地区映射；未猜测美国/香港/日本默认配置。
- Anthropic 的版本行为见 [官方版本说明](https://platform.claude.com/docs/en/api/versioning)。当前源码仍发 `2023-06-01`，没有擅自切到更新版本。官方要求的 `x-api-key` 与现有网关一致；不含 federation、云平台认证或 beta 功能。
- Gemini 使用 [官方 API key Header](https://ai.google.dev/api) 与 [GenerateContent API](https://ai.google.dev/api/generate-content?hl=en)。当前 quickstart 转向 Interactions，不意味着已实现该新 API；这里明确保留 GenerateContent 模式。
- OpenAI 的 [API 概览](https://developers.openai.com/api/reference/overview) 提供 Bearer 认证；Responses 为本预设默认模式。Chat 也由现有网关表达，但用户切换协议后仍须核对所选模型的端点支持。不自动补组织/项目 Header。
- OpenRouter 认证见 [官方认证](https://openrouter.ai/docs/api/reference/authentication)；NVIDIA Bearer 用法另见其 [官方客户端配置](https://github.com/NVIDIA/OpenShell/blob/main/providers/nvidia.yaml)。均不代表目录中的所有模型已获授权。
- LM Studio 默认可无认证；启用 token 后按 [官方认证](https://lmstudio.ai/docs/developer/core/authentication) 配置 Bearer token。vLLM 的 [官方 OpenAI 服务说明](https://docs.vllm.ai/en/latest/serving/online_serving/openai_compatible_server/) 支持 `--api-key` / `VLLM_API_KEY`，端口与模型运行条件以实际启动配置为准。本地运行、模型下载和模型加载均不由预设自动完成。
- 模型刷新沿用现有目录实现，通常请求相同 base 的 `/models`，Gemini 使用对应模型目录格式。推理 API 兼容不能证明 `/models` 必然可用、完整或代表当前账号权限；目录不支持/失败时须明确反馈并允许手动模型 ID，不伪造静态模型目录。

## 明确不包含

Azure OpenAI、Amazon Bedrock、Vertex AI 等需要额外认证、资源/部署或区域参数的模式没有放入本表。未声称 OAuth 登录、消费者订阅、Coding/Token Plan、自动续期、多账户、额度/余额或计费能力已经实现。Z.ai、Moonshot 中国站、MiniMax 中国站、SiliconFlow 国际站等不能通过替换品牌名称隐含支持，需另行核对对应地区文档、账户和端点。

## 本地验证

模块内测试不读取账号环境变量、不使用真实凭据、不联网：检查通用数据类型的字段校验、四协议名称可解析、非法 URL/来源被拒绝；读取本仓库随附 TOML 包，检查 manifest 结构、插件/预设 ID 唯一、所有默认项通过现有 `Provider::validate`、无 key 默认项仅指向 loopback。测试结果与 Registry 启停/界面集成/外部账号验收分别记录。


2026-11-02 本地结果：`cargo +1.99.1 test -p magpie-core --lib provider_presets` **4 passed**。Python `tomllib` 另核对 **20 个插件 ID、21 个预设 ID、四种协议**及 URL 结构；定向 rustfmt 检查通过。产物写入外部 `/home/lightjunction/.cache/magpie-rs-build/smoke-20261102/target-cli`，`CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0`。初次构建遇到其他 worker 尚未完成的 Agent 模块错误；其源码写入后重跑通过，本任务未改 Agent 文件。

严格 `cargo +1.99.1 clippy -p magpie-core --lib -- -D warnings` 未通过：其他 worker 的 `agent_connection/native.rs:527` 存在未使用函数 `write_new_private`（dead_code）；未扩大写集修改。

共同 schema 已与 Registry worker Feynman 确认：保留 `[[contributions.providers]]`，多个插件可共同提供 `provider-preset`；Registry 保留原始 `DocumentMut` 供宿主解析，真实注册成功后才确认 activation。补齐共同元字段后上述四项测试再次通过。

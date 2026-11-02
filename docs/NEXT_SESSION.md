# 当前接续进度 · 2026-11-02

已将远端 main 的共享 Provider、系统凭据、偏好持久化、监督网关、Codex 连续换模与 Material 实现整合。旧环境变量 Provider 原型和旧网关进程实现已由共享实现取代；原草稿仍可从 500c441/36086d0 及 wip/usage-recording-20261102 恢复，不把未接入 Usage 文件算成实现。

配方保留远端 1.4.0 的全部 99 REQ，并升级到 1.4.1。ISSUE_FEEDBACK.md 记录本项目/源码仓库/Material 无 issue 的查询与三条相关上游报告，区分已修复 #4343、待确认 #4851/#4855 和本地复现。下一阶段优先检查刷新时保留显式模型选择、流内错误归属及 Usage 账务，随后贯通路由和更多 Agent 配置；完整功能与平台目标没有缩小。

## 本轮验证

- workspace 329 passed、4 ignored；Material 桌面含交互特性 80 passed、3 ignored。真实监督网关 owner 崩溃测试通过，CLI 无 GUI 启动依赖。
- 80 个中文真实 GPUI Scene（宽/中/440/360px、浅深主题、10 场景），渲染导出推进模拟时钟，避免把 Material 导航入场动画的透明初帧作为截图。
- Linux Wayland 原生窗口：供应商新建/刷新后，从 CLI 和实际 Gateway 读到相同模型；真实合成 HTTP 请求返回 fixture-ok；Codex 预览不改文件，连接 a → 直接换 b → 断开恢复最初配置的原始字节，未知字段保留。主题/语言偏好实际保存到磁盘，网关原生停止释放端口、重启产生新实例。
- 取消和窗口关闭的未保存草稿确认已接到共享 Provider 页面，继续编辑保留内容；明确放弃才擦除尚未保存的密钥。组件回归与原生窗口已检查。
- 原生 AT-SPI 仅返回应用根，不能宣称读屏通过。自动注入 ASCII 时会被本机拼音组合拦截，结束组合并切直接输入后 Ctrl+A/Tab 正常；这不等同完整 IME 验收。用户对当前视觉尚未认可，全部其他页面、真实账户和多平台仍未完成。

日志/像素/合成状态位于 /home/lightjunction/.cache/magpie-rs-build/smoke-20261102/。使用 scripts/desktop-fixture.py --cli PATH --desktop PATH --output NEW_EXTERNAL_DIR 启动隔离窗口；--reuse 复用本脚本的合成目录进行重启检查。真实认证目录被 bubblewrap 隔离，所有 API 调用只到 loopback fixture。未发布软件包，提交均跳过 CI。

## 后续增量：窄窗键盘与流内错误

原生 360px 德语窗口发现默认 Tab 绕过表单滚动，已改为 Provider 上下文专属的键盘 Action 与完整字段顺序；不可编辑的 ID 跳过，URL/协议/API key/env/model 通过 Tab 自动滚入可见区，Shift+Tab 可返回。实际 Kit Root 的焦点回归与原生滚动分别验证，源码机器证据见 tests/results/keyboard-stream-2026-11-02.json。

对原版 #4855，32 个协议配对/前缀以及 12 个无终态/断流/取消 fixture 未复现 HTTP200 被误计成功或重播，当前 transport counters 区分失败与取消。实际修正是将 upstream_timeout 从通用 502 映射为 Timeout/504；原生 passthrough 的 code/status/阶段诊断、Usage/插件账本仍未实现，不能登记为原版 issue 全部修复。

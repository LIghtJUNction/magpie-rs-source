# 下次接续：Material 与原版功能对齐

## 推送分支与合并前提

收尾推送时发现远端 main 已有另一组 Provider、系统钥匙串、监督网关与 Codex 模型更新实现，当前工作从较旧的 f20439b 基线开始，双方已经分叉。因此本轮源码保存到 `wip/material-desktop-20261102`，配方仓库同名分支指向本轮源码；不覆盖远端 main。文档中的功能与测试状态仅对应本轮 checkpoint，不代表远端 main 的完整能力。

下次先对照远端 main 与本分支，保留两边有效工作，在更新的 Provider/网关实现上整合 Material UI，再跑完整测试。不要直接采用某一方文件覆盖另一方，也不要重复开发远端已有功能。


2026-11-02 用户明确反馈“太丑了”“功能比原版少了不少”，随后要求尽快提交推送、结束，下次继续。此 checkpoint 保存工作，不代表用户认可 UI，也不是完成原版重建。

## 当前已经落地

- 保留并提交本轮开始时已有的 Provider 管理与协议转换工作；涉及 core、service、CLI 与 GPUI，未删除既有功能。
- 组件库固定 `mizorewww/gpui-kit-material@47895b92558883d24e19d54fdeb876ed551d9394`，真实 MaterialButton、NavigationRail、主题与图标资产已接入；桌面工具链 1.99.1，独立 CLI 工作区保留。
- 新增窗口拥有的 Gateway 子进程：后台启动、绑定回执、端口冲突失败、退出监测、停止并回收、窗口销毁时清理；外部服务不被接管/终止。
- 初版在 Linux Wayland 真实打开过。用户拒绝该视觉方案；随后修复系统主题初始状态与观察回调、调整导航断点、字号与容器。这些后续更改尚未重新完成实际窗口验收。

## 已验证与未验证

- 初始同一业务源树：workspace 293 passed，5 个 helper ignored；Provider 合并回归 18/18，包含本地跨协议请求。
- Material 桌面库：77 passed，2 ignored；另显式运行真实 CLI 网关所有权/停止/端口冲突/Drop 清理用例，通过。
- 收尾重新检查 desktop 全目标与 visual-test 示例。正式发布包、其他操作系统和真实账号未验证。
- 原始日志与隔离 runtime：`/home/lightjunction/.cache/magpie-rs-build/smoke-20261102/`。不使用用户真实 ~/.codex；测试 key 为合成数据。

## 下一次优先做

1. 先读父配方 MATERIAL_UI.md、COVERAGE.md 与完整递归规格。保留全部 93 条需求。
2. 对照原版 `yetone/magpie/site/public/img/agents-light.png` 与实际页面。保留“Agent 行列表、行内模型、连接开关”的信息结构；解决空散布局与字体、间距、主题一致性后重新打开窗口检查，不把换组件等同于重构验收。
3. 核心缺口是多 Agent 的真实配置连接（当前只有 Codex），不是加导航入口。Claude/OpenCode/Gemini 的格式、未知字段与恢复契约需逐项实现和验证。
4. 路由模块目前是纯决策基础，尚未接入 Gateway、持久化 CRUD 和真实 failover。Usage、Profiles、订阅账户、Library、插件和同步也未交付。
5. 未接入的 Usage 原型单独保存在源码分支 `wip/usage-recording-20261102`。它不是产品功能；需要补依赖、挂载模块、接入实际请求/流终态、隐私与去重测试和 UI，再合并。
6. Settings 仍只作用于当前窗口，偏好持久化未做。Provider 目前用环境变量名引用密钥，不支持粘贴密钥至系统钥匙串或订阅登录。

## 发布边界

本轮只提交源码和配方进度，不触发 CI、不生成发行版；源码须先推送，再更新父仓库 gitlink。GitHub 安全页的 5 条历史告警已按测试数据/公开安装式 OAuth 常量关闭；Dependabot 为 0，Code scanning 没有分析记录，不能称已通过源码安全扫描。

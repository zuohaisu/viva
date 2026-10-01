# ADR 0012 — 常驻运行时、终端编排 API 与退出暂停语义

Status: **Accepted**（2026-10-01，Haisu 在 herdr 对齐研究中逐项裁定；随 PR #41 归档）· Implementation: 未实现；切片序列见 [herdr 对齐研究](../research/herdr-runtime-parity-2026-10-01.md) §6（重排版）。

> 效力说明：本文修订 [ADR 0011](0011-rust-host-and-tui.md) §6（关闭界面的语义）并补充其 §5.4；ADR 0011 其余条款（Rust 宿主、Pi 宿主、SQLite 状态层、资源与验收纪律）继续有效。裁定过程与来源固定见研究文档 §5.1–§5.3。

## Context

herdr 对齐研究（2026-10-01）确认：终端复用器形态的成熟参照物 herdr（Apache-2.0，约 41.6k stars）建立在"常驻 server 持有全部终端会话、TUI 仅为可分离客户端"之上。用户逐项复审后裁定：detach/attach（关窗口 agent 继续跑）、类型二活恢复（server 重启/升级期间 agent 进程不中断）、live handoff、SSH 多机（后续阶段）为 Viva 需要的能力；并裁定 Viva 的产品方向包含"agent 编排另一批 agent"，需要终端原语编排面。这些裁定共同指向常驻运行时，推翻 ADR 0011 §6 当时的"无常驻 daemon"裁决；该裁决的动机（生命周期复杂度、资源预算、可预期性）被显式接受为成本，交换上述产品能力。

用户同时澄清权威状态的来源结构：Viva 生态中的自模型容器必须经受控通道取得 agent 内容（在 claude 等工具里跑了什么、形成了哪些总结、做了什么任务），交容器判定是否保存；屏幕推断不作为默认权威，仅作展示辅助。

## Decision

1. **常驻运行时 server**：`viva` 分为常驻 server 与客户端。server 持有 PTY、终端仿真状态、有界日志、socket 面、任务派发与维护调度；TUI 与 CLI 为客户端。V05 确立的进程所有权纪律整体沿用：进程组回收、graceful-first 停止、确认 reap、不误杀非拥有进程；server 自身崩溃的恢复协议在实现切片中验收。
2. **detach/attach 与两级恢复**：客户端随时断开/重连。类型二（活恢复）：server 重启/升级场景实现 fd 级活转移，agent 进程不中断；转移失败退回类型一。类型一（事实恢复）：关机、断电、系统重启后按 ADR 0011 §6 原语义恢复——SQLite 恢复成员/任务/执行/归属事实，恢复动作显式触发，agent 原生会话经 harness 能力 resume。
3. **退出与暂停语义（取代 ADR 0011 §6 的"关闭即暂停"）**：关闭 TUI 后任务派发与定期维护**默认继续**；必须提供显式 pause/resume（CLI 与 TUI 双入口）；暂停 = 不接新派发、不跑维护周期，已运行执行不受暂停误停（停单个执行用既有 stop 语义）；提供 `close-policy: continue | pause` 配置，默认 continue。
4. **终端原语编排 API**：本机 socket 面（NDJSON）提供 sendKeys、wait、prompt、事件订阅及会话/pane 管理原语，作为"agent 编排另一批 agent"的基础设施。**所有 socket 调用经 Viva 授权校验**（grant 语义延伸到 socket 层），不得成为绕过授权的旁路；worker 请求不得冒充用户（ADR 0007、宪章 Actor-Aware Delivery Authority 继续适用）。本条与 ADR 0011 §5.4 的"RPC/SDK 未来可选"区分：后者指 Pi 内部 RPC/SDK（接管 Pi 聊天循环），本次不激活、状态不变。
5. **agent 状态来源结构**：权威状态只来自受控申报——pi 经扩展申报；提供钩子机制的第三方 CLI（claude 等）逐步集成上报；受控通道同时承担自模型容器的取材职责（容器侧策展规则属自模型边界，Viva 保证通道与留痕；raw event 不自动成为 memory，宪章本体论不变）。屏幕推断仅作展示辅助，不进事实库、不作验收与派发依据；ADR 0011 §5.2 保持不变。首版检测范围 7 个 CLI：codex、claude、codebuddy、qodercli、cline、hermes、pi coding agent；其余按需增补。
6. **首版范围与明确不做**：范围内——多 pane 布局、worktree 分组工作台、无头固定尺寸终端、音效与 toast 通知。明确不做——插件与市场、onboarding 向导、面向 agent 的 SKILL.md、键位自定义、kitty 图片协议渲染（留给后续桌面客户端）、CJK 输入法光标跟随专项（生态成熟或实际暴露问题再立项）、主题系统与 UI 国际化（后续加入）、SSH 多机（后续阶段必须，首版不实现）。

## Consequences and validation

- 资源验收（V12）扩展：常驻 server 稳态内存、多客户端连接、活转移路径纳入 16 路并发门槛；Intel Mac 预算重新核对，不沿用旧假设。
- 测试矩阵扩展：两进程竞态（attach/detach/stop/wait/exit 交错）、fd 转移失败路径、socket 未授权调用被拒且留审计。
- 语义验收目标不变：零越权、零误杀、零归属丢失、零完成任务重放（ADR 0011 §6 原目标）。
- 许可：路线 A 零复制基线不变（研究文档 §7）；借鉴思路不复制代码；若未来出现复制，按该节规则随附 THIRD_PARTY_NOTICES。
- 估计纪律：研究文档初版给出的规模估算已因范围扩展作废，不编造新数字；以切片验收记录累积实测。

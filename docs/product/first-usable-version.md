# Viva 首个可用版本：独立并行开发环境

Status: **Accepted product requirement / implementation pending** · 2026-09-28 · Owner: Haisu

来源：Haisu 明确要求首个可用版本至少替代 Orca 的多 worktree 并行开发，否则没有独立使用入口。本页定义投入使用门槛，不声称现有 Python 或规划中的 Rust 已达标；Rust/Pi/SQLite 技术裁决见 [ADR 0011](../decisions/0011-rust-host-and-tui.md)，执行计划见 [rollout](../implementation/rust-office-rollout.md)。本页修正旧 Orca 研究中“Viva 只作连续性层、不负责终端编排”的产品边界，不改变成员/任务/执行本体。

## 1. 使用入口与所有权

从 Terminal.app/iTerm 等普通终端启动一次 Viva，进入同一个本地 Viva 实例。协调成员对话、多个 workspace/project、多个开发任务与它们的终端都由该实例管理。切换项目只是导航，不新启 Viva，也不改变正在执行任务的归属。

- 同一 VIVA_HOME 由一个活跃 Viva 宿主持有终端句柄；再次启动明确提示/定位已有实例，不重复启动同一任务。不同 workspace 不强制不同 Viva 进程。
- 不依赖 Orca 安装、运行或 Orca CLI；不在每个 worktree 内再起一个 Viva。外部 Agent CLI 是 Viva 的子执行工具。
- 协调对话可以没有 Task；开发任务拥有隔离 worktree。一个 Task/worktree 可以挂多个用途不同的终端（Agent、shell、测试），辅助 shell 由用户拥有，不捏造 AI 成员或第二套 Task。
- 关闭 TUI 后暂停派发/维护并停止 owned 执行；保留 worktree、用户改动、任务结果和 harness 会话引用。重启对账后由用户明确继续，不自动重跑、拉起 daemon 或承诺原生 Agent 能从精确指令位置恢复。

## 2. 独立替代的最小能力

| 用户动作 | 首版必须成立 | 交付职责 |
| --- | --- | --- |
| 打开仓库/项目、建并行任务 | 创建或发现并选择现有 worktree，显示路径/branch/任务归属；发现不等于接管或删除 | V02/V03/V08 |
| 启动开发工具 | Pi 默认；其他已安装 Agent CLI 用显式 argv/cwd 启动，不要求都有 Pi 的成员扩展 | V05/V07/V09 |
| 同时开发多个任务 | 各写各 worktree；TUI 列出所有任务与终端；等待输入/运行/退出来源可解释 | V06/V14 |
| 在 Agent/测试/shell 间操作 | 键盘选择、输入、resize、scrollback；同一 worktree 多终端，焦点不串 | V05/V06/V14 |
| 检查代码与交付情况 | 真实 git status/diff、branch、PR/checks；可调用现有编辑器和普通 shell/gh，缺认证如实显示 | V08/V14 |
| 暂停或停止一个任务 | 不影响其他任务；队列/资源不足有可行动原因；Task 完成与进程退出分开 | V03/V04/V05/V07 |
| 退出/重开后接着工作 | worktree/改动/任务/终端引用可找回，误杀、重复启动和完成任务重放为验收失败 | V07/V12/V14 |

状态面板是现有 Task/Execution/终端事实的投影；不再引入一套 todo/in-progress/completed worktree 工作流。没有结构化 harness 事件时只显示可证的“进程运行/退出、输出更新时间、用户标记需处理”，不靠输出安静猜“Agent 已完成/等待授权”。

检查 diff 与调用编辑器是日常开发入口，不扩张为编辑器、调试器或桌面 IDE。用户自己在 shell 使用 Git/gh 完成已有权限下的开发与 PR；Viva 原生 GitHub 查询入口仍只读。开发 Agent 的分支 push/PR 遵守现有 owner 授权契约，不从本页获得 merge/approve/受保护分支权限。可配置自动 Dev→QA→交付 workflow 是 F01 后续，不能拿它缺席为无法手动并行开发的理由。

## 3. 真正投入使用的验收

V14 定义日常开发演示，V12 独立采样与复核，V13 将其纳入发布门槛：

1. Orca 关闭、Viva 不调用其 CLI；从普通终端启动一次 Viva。
2. 在一个真实低风险仓库中安排至少 **3 个并行开发任务**，各自独立 branch/worktree；其中真实 Pi 和另一种已安装 Agent CLI 同时运行。新工具不支持 Pi 扩展时仍可正常人工交互，不能假装拥有同等委派/分叉能力。
3. 在其中一个 worktree 再开测试/shell 终端，键盘来回切换，观察结果和真实 diff。输入/路径/结果归属不串；停止一个执行，其他执行继续。
4. 由用户或已获现有明确授权的开发 Agent 完成实际低风险修改、验证、独立 review 及必要分支/PR 操作；PR/head/checks 能在 Viva 找到证据。没有授权就停在待 owner 状态，不要求为演示合并自己的 PR。
5. 关闭 Viva 后无 owned 进程残留，worktree 与未提交改动仍在；重新打开能找到原任务、会话引用/交接与输出，明确选择继续，不重复执行完成任务。
6. 记录实际机器、工具版本、模型和证据；Intel Mac 与约16真实 Agent 峰值资源预算由 V12 验收，缺硬件/凭证保持 pending，不拿模拟或 Apple Silicon 数据代替。

“至少3任务”证明日常并行工作流；“约16真实 Agent”验证峰值资源预算，两者都不是16个开发构建进程。当前不存在上述运行证据。

## 4. 复用判断

复用包括直接依赖成熟库、抽取许可证允许且能脱离原应用的代码、借鉴行为/测试；不等同于要求用户继续运行原应用。Orca 固定源码/许可证及候选路径见 [reuse audit](../research/orca-reuse-audit-2026-09-28.md)。实现者必须记录选取模块、耦合、许可证、生命周期与预算证据；不凭 MIT 标签宣布 Electron 模块能直接放进 Ratatui。

V14 只补现有领域/PTY/TUI/Git模块之间缺失的日常工作台组合，不另建调度框架。复用 Orca CLI 可以是未来可选 driver，不能用它通过本页的独立替代验收。

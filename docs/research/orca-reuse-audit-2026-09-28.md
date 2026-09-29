# Orca 源码复用审计与 Viva 独立并行开发边界

Status: planning evidence; source inspected, integration not built or benchmarked. Date: 2026-09-28.

[Goal check] This work advances Viva's execution/runtime capability by identifying licensed existing worktree and terminal capabilities and the concrete gaps to independent parallel development.

## 1. 定位、固定来源与许可

审计对象是本机 `/Applications/Orca.app` 的 `com.stablyai.orca`，不是同名屏幕阅读器。本机 `Info.plist` 记录 1.4.212，`Resources/app-update.yml` 指向 `stablyai/orca`；`Resources/orca-local-build.json` 记录 arm64 构建和 commit `841503152c6a0a6885566bd8b0a21b2bde4a7aa1`、daemon protocol 36。本次只读应用安装信息与依赖 notice，没有读取用户会话、状态库或凭证。

官方公开仓库包含应用源码、测试和构建配置。固定 commit 的 [LICENSE](https://github.com/stablyai/orca/blob/841503152c6a0a6885566bd8b0a21b2bde4a7aa1/LICENSE#L1-L21) 为 MIT，版权主体为 Lovecast Inc.。允许复制、修改和分发；实际复制代码及实质性材料时须保留其版权及许可声明，另行保留第三方许可。Viva 的 MIT 不代替这些声明。[官方仓库](https://github.com/stablyai/orca) 的公开源码与 MIT 声明也经过实时读取。

下面全部源码链接固定到安装元数据所指 commit。该 commit 的 [package.json](https://github.com/stablyai/orca/blob/841503152c6a0a6885566bd8b0a21b2bde4a7aa1/package.json) 自报 1.4.197，与应用显示 1.4.212 不同：这里记录两份第一方证据，**没有证明二进制与此源码完全可重现一致**。实时仓库首页还显示另一个 main commit `27b823f934f739bc85914dd717b776835f60bcf7`；本次未混用它的实现来证明安装版本能力。

## 2. 首版边界：复用执行能力，不依赖 Orca 的运行环境

用户明确要求首版能够替代 Orca 的多 worktree 并行开发工作面。验收因此须在不启动 Orca、不连接它的现有 profile/runtime/socket 的条件下完成：一个 Viva 实例列出多个工作上下文，在不同 worktree 启动不同 agent CLI，切换和交互，观察真实进程/等待输入状态，保存会话关联，停止指定进程并在重启后按能力恢复。

这个目标允许 Viva 内部监督一个打包的执行 helper；它不等于所有组件都必须 Rust。反之，调用用户现有 `orca worktree` / `orca terminal` 虽是有效复用适配，却不能作为独立替代验收通过。选择 helper 之前必须核对所有权、退出策略、资源与发行证据。

## 3. 可复用模块和耦合

| 能力 | 已检查源码 | 复用类别 | Viva 仍需填补的具体缺口 |
| --- | --- | --- | --- |
| Git/worktree 列举 | [porcelain parser](https://github.com/stablyai/orca/blob/841503152c6a0a6885566bd8b0a21b2bde4a7aa1/src/shared/git-worktree-porcelain-parser.ts#L1-L81) | 小函数可抽取或移植；直接调用成熟 git CLI | NUL/引用路径、main/locked/prunable 信息应测试；列表不是 Viva Task/成员归属。Rust 实现可复用行为和案例，不必加载 TS runtime。 |
| worktree 创建与并发修改 | [worktree-add](https://github.com/stablyai/orca/blob/841503152c6a0a6885566bd8b0a21b2bde4a7aa1/src/main/git/worktree-add.ts)、[worktree operation lock](https://github.com/stablyai/orca/blob/841503152c6a0a6885566bd8b0a21b2bde4a7aa1/src/shared/git-worktree-operation-lock.ts) | 语义和小代码可复用；完整创建模块耦合 | 创建模块还依赖 Git runner、ref maintenance、cache 和 WSL routing。Viva 需自己的 operation/Task 映射、取消和授权；锁的对象要按实际 Git 共享资源区分，不能把 worktree 路径锁当所有 repo ref 安全的证明。 |
| PTY 创建、输入、resize、生命周期 | [TerminalHost](https://github.com/stablyai/orca/blob/841503152c6a0a6885566bd8b0a21b2bde4a7aa1/src/main/daemon/terminal-host.ts#L1-L105)、[Session](https://github.com/stablyai/orca/blob/841503152c6a0a6885566bd8b0a21b2bde4a7aa1/src/main/daemon/session.ts)、[native spawn](https://github.com/stablyai/orca/blob/841503152c6a0a6885566bd8b0a21b2bde4a7aa1/src/main/daemon/pty-subprocess/native-pty-spawn.ts) | 可切出 Node 执行 slice 的候选；不是已发布的独立 TerminalHost 库 | TerminalHost 注入 spawn，并管理 creation fencing、session owner 与 tombstone；Session 再依赖 output pipeline、shell readiness、termination 和 startup ingress。native spawn 用 node-pty，还带 macOS TCC、Windows job/fallback。需证明独立打包、边界 API、停止单个进程树与 owned cleanup。 |
| 终端历史与重启恢复 | [terminal-history-log](https://github.com/stablyai/orca/blob/841503152c6a0a6885566bd8b0a21b2bde4a7aa1/src/main/daemon/terminal-history-log.ts)、[workspace session schema](https://github.com/stablyai/orca/blob/841503152c6a0a6885566bd8b0a21b2bde4a7aa1/src/shared/workspace-session-schema.ts)、[workspace session controller](https://github.com/stablyai/orca/blob/841503152c6a0a6885566bd8b0a21b2bde4a7aa1/src/main/runtime/runtime-workspace-session-controller.ts) | framed log 的恢复规则可移植；整个 workspace UI schema 不宜照搬 | log 的长度帧可识别 torn tail。schema/controller 同时描述 tab/layout/editor/browser、host partition 和 runtime store，不能成为 Viva 的第二份会话事实。保存输出也不等于恢复 LLM context；恢复须用 harness 原生 session 标识及支持能力。 |
| agent 命令、模型选项与启动 | [TUI_AGENT_CONFIG](https://github.com/stablyai/orca/blob/841503152c6a0a6885566bd8b0a21b2bde4a7aa1/src/shared/tui-agent-config.ts)、[launch command resolver](https://github.com/stablyai/orca/blob/841503152c6a0a6885566bd8b0a21b2bde4a7aa1/src/shared/tui-agent-launch-command.ts#L1-L99)、[launch executor](https://github.com/stablyai/orca/blob/841503152c6a0a6885566bd8b0a21b2bde4a7aa1/src/main/agent-launch/agent-launch-executor.ts) | catalog/argv 规则可抽取或作为适配测试来源；executor 耦合 runtime/surface/workspace | resolver处理shell、覆盖命令、model/effort 参数冲突。executor 的注释明确只统一 agent.launch，其他启动面仍分散。Viva 只需首版明确支持的 CLI，不复制整个 native-chat/renderer 模型目录；状态和参数必须匹配实际安装工具。 |
| agent 状态持久化 | [bounded status serializer](https://github.com/stablyai/orca/blob/841503152c6a0a6885566bd8b0a21b2bde4a7aa1/src/shared/agent-status-store-persistence.ts) | 有界解析/序列化设计可复用 | 状态快照依赖 Orca contract/codec。Viva 必须分别呈现进程存在、工具可交互、等待输入、退出和 Task 验收；不能把空闲/退出0解释成 QA 通过或交付完成。 |
| desktop 终端渲染 | [package.json](https://github.com/stablyai/orca/blob/841503152c6a0a6885566bd8b0a21b2bde4a7aa1/package.json) | UI 和 renderer 主要借鉴设计 | Orca 是 Electron/React/TypeScript 应用，使用 node-pty 和 @xterm/headless/addon-serialize；这些 JS 依赖并不提供 Ratatui widget。Ratatui 嵌入终端需要解析终端状态及单一渲染所有权，不能把 desktop WebGL 终端直接放进物理 terminal。 |

以上是静态源码审计，**没有复制实现、执行 Orca 测试或建立 Rust 适配**。已有 Viva worktree、脱敏与权限能力仍是先检查的复用来源；语言替换不废弃这些有效契约。

## 4. 被发现的候选：不加载 Electron 的 orcad

[build-orcad.mjs](https://github.com/stablyai/orca/blob/841503152c6a0a6885566bd8b0a21b2bde4a7aa1/config/scripts/build-orcad.mjs#L1-L58) 明确提供 plain Node bundle，入口为 [orcad/main.ts](https://github.com/stablyai/orca/blob/841503152c6a0a6885566bd8b0a21b2bde4a7aa1/src/main/orcad/main.ts)。构建脚本 target 是 node18，并检查 runtime、daemon、watcher 的打包依赖图不引入 Electron；node-pty/@parcel/watcher 是外部 native 依赖。这个 target 不能替代实际 Node/ABI/平台兼容测试；仓库开发 engines 则要求 Node24。

[orcad-entry.ts](https://github.com/stablyai/orca/blob/841503152c6a0a6885566bd8b0a21b2bde4a7aa1/src/main/orcad/orcad-entry.ts) 安装 Node app environment/secret-store 适配，构造与 desktop 共用的 OrcaRuntimeService，注册 headless PTY 控制面并提供 RPC。它仍是应用 runtime 图的打包入口，**不是已经验证的小型嵌入 SDK**。其 secret store 还明确返回加密不可用，不能因去掉 Electron 就假设继承 desktop credential 保护。首版执行 helper 应避免接管和复制用户凭证，使用 CLI 的原有认证。

[orcad-daemon-supervision.ts](https://github.com/stablyai/orca/blob/841503152c6a0a6885566bd8b0a21b2bde4a7aa1/src/main/orcad/orcad-daemon-supervision.ts#L1-L67) 的 PTY daemon 被 detach，以使终端活过 runtime 重启；`stopOrcadDaemon()` 只 disconnect，不 shutdown。这个行为与 Viva 既定“关闭 TUI 后暂停任务和定期维护、停止 owned 进程”不能直接画等号。采用整段 runtime 或 slice 前必须显式实现 Viva 所有权和退出协议，包括强制退出、helper 故障、孤儿回收，且不能误杀其他终端。

本机 node-pty 的 package 与 LICENSE 也只读核对；其 MIT notice 独立于 Orca MIT。直接复用 node-pty 是 **Node helper 路线**，不是 Rust 可直接链接的纯 Rust 库。@xterm 等其他依赖的各版本完整许可清单本次未审计，不能把 Orca MIT 视为所有依赖许可的总证明。

## 5. 对已规划 issue 的裁决建议

Rust 宿主的决策无需撤销。先在终端执行 issue 中设置一个短、可证伪的集成门槛，对同一 API 比较两条路径：

1. 成熟 Rust PTY/终端解析库 + 最小 Viva 生命周期 glue；所选库的版本、源码与许可还需在该门槛实际核对。
2. 固定 Orca commit 的 Node-only terminal slice，或受 Viva 监督、隔离 data root 的 orcad helper；保留 MIT notices，限定功能图，不运行现有 Orca app。

helper 应由一份 Viva 实例共享，不为每个 Task 启动一整套平台；IPC 的请求/输出队列需有上限和背压。V05 的选路门槛输出应是最小 runnable proof、依赖/发行清单及同机测量：真实 PTY 输入/resize/paste、错误启动、指定停止、helper 退出与 owned cleanup、内存/IPC/响应，不等待整个工作台完成。随后 V14/V12 的整体门槛验证三个隔离 worktree、Pi 与另一种真实 CLI、shell 命令、切换、Viva 退出、重启关联/resume；不能把全产品体验反过来作为底层选路前提，也不靠语言优势推断。Mac Intel 的最终容量验证依然属于资源验收；没有目标机器不能填 PASS。只把明确定义的能力门槛作为依赖，不让整个后续资源 issue 阻塞所有实现。

Git/worktree issue 直接围绕 git CLI、现有 Viva 契约及 Orca 的路径/锁/恢复反例收敛；TUI issue 必须交付多 worktree 导航和真实交互，而不只是 Samuel 单会话；发行/总体验收必须要求无需 Orca 安装。Orca optional driver 可以保留作为方便的外部适配，但不算独立替代证据。

## 6. 已验证与未验证

已验证：安装产品的准确定位与版本元数据；官方仓库有源码且固定 commit LICENSE 是 MIT；具体 Git/PTY/session/agent 模块和静态耦合；存在 Node-only runtime 构建入口；其 detached-daemon 默认生命周期与 Viva 退出要求不同。

未验证：源码到本机二进制的可重现映射；orcad 或 slice 在 Viva 下构建/独立安装/协议适配；Ratatui 终端兼容；资源、速度和16并发；Intel实机行为；跨 harness resume 的实际效果；第三方全量许可/安全审计。研究工件不构成实现、性能 PASS 或首次独立使用成功。

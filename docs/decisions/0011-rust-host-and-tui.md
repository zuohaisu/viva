# ADR 0011 — Rust 宿主与终端 TUI

Status: **Accepted**（Haisu 显式裁决，2026-09-27）· Implementation: **Not implemented**

> 效力说明：宿主采用 Rust 是用户裁决；Tokio + Ratatui + Crossterm 是随此方向记录的实施默认组合。本文是技术选型的唯一权威结论，历史研究只保留来源与讨论过程。当前可运行版本仍是 Python 3.11+ / Textual，本次只更新文档。

## Context

Viva 是本地优先 Personal AI Office。成员身份、任务、授权与工作历史必须独立于模型、工具和进程存在（ADR 0006–0010）。用户故事需要：协调成员作为入口、多项目 workspace、谈话分叉、知识退出机制、模型与工具绑定、任务 worktree、可复用工作流、技能策展、维护，以及浏览器和原生应用操作。

首版以终端 TUI 为主。常态为一个或少量执行，偶尔在同一台机器上并发约 16 个 Agent；目标还包括内存较小的 Intel Mac。这是应用运行时负载，不是每轮开发或 CI 必须启动 16 个编译实例。未来桌面与 Windows 客户端只影响接口边界，本次不选择桌面框架。

用户明确：迁移成本、实现复杂度不作为否决条件；资源占用、交互速度、真实任务完成速度和构建反馈时间需要考虑。关闭 TUI 后，任务与定期维护应暂停。

## Decision

### 1. 技术组合与范围

| 层 | 决定 | 范围 |
| --- | --- | --- |
| Office 宿主、CLI、执行监督 | **Rust** | 拥有成员/任务/执行/授权关系、恢复与资源生命周期 |
| 异步 I/O 与监督 | **Tokio** | 等待模型、工具和进程；阻塞工作与绘制分离，队列有界 |
| 终端界面 | **Ratatui + Crossterm** | 一个终端渲染者，CLI/TUI 共用 Office 能力接口 |
| 认知与 Agent 工具 | **复用现有实现；具体接入待定** | Pi SDK/RPC、现有 CLI/MCP 是候选，不自建通用 Agent harness |
| 存储 | **另行裁决** | 当前 JSON/JSONL/Markdown 是基线；SQLite 是候选，不随语言裁决自动通过 |
| 浏览器/原生应用操作 | **复用成熟能力，按 driver 接入** | Playwright、系统 API/小型 helper 是候选；具体方案待验证 |
| Desktop / Windows | **未选型** | 保持 domain 与 surface、平台 driver 分离，无首版实现承诺 |

用户从 terminal 启动 `viva`，程序留在 terminal 中运行。目标发行方式是对应平台的可执行程序，最终用户不需要安装 Rust 编译器；外部 Worker、浏览器及系统权限仍有各自前提。开发与 CI 需要 Rust 工具链，具体最低版本和依赖版本在实现时固定。

Rust 决定的是 Office 宿主，不要求把 Pi、浏览器引擎、模型 SDK 或所有 helper 重写为 Rust。`Viva ≠ Samuel`，成员名与职责仍为配置数据。

### 2. 为什么选 Rust

第一性需求是：让办公室的事实持续存在，以小且受控的本地运行时监督可替换工具，并让交互在外部任务等待或大量输出时保持响应。

Rust 的优势是显式所有权与资源生命周期、无需 tracing GC 的宿主、原生可执行文件，以及访问操作系统能力的路径。用户不以实现难度淘汰候选，因此可以接受 Rust 的开发与构建代价，换取宿主层对资源和边界的控制。这是工程选择，不是已测得的速度冠军。[Rust 所有权](https://doc.rust-lang.org/book/ch04-01-what-is-ownership.html)

Pi 的轻量体验与可复用核心支持“复用 Agent 能力”，并不要求 Viva 的 Office 宿主也使用 TypeScript。反过来，采用 Rust 不证明 Viva 会比 Pi 或 Claude Code 更快：模型延迟、上下文量、工具调用、扩展加载、浏览器和外部 CLI 都可能主导任务耗时。当前没有 Viva 候选栈在目标 Intel Mac 上的同负载基准。

### 3. 候选比较与代价

| 候选 | 核心优势 | 核心代价 / 本次取舍 |
| --- | --- | --- |
| **Rust + Tokio + Ratatui/Crossterm** | 无 tracing GC；资源生命周期明确；原生分发；适合执行与平台边界 | 编译和链接反馈可能较慢；异步取消、FFI、进程树回收仍需正确实现；本次选择 |
| **Go + Bubble Tea** | 单二进制；并发与 TUI 成熟；通常有较短构建反馈 | GC 与堆预算需要考虑；不代表性能不合格；是可行替代，暂无同负载数据证明输给 Rust |
| **TypeScript + Node + Pi TUI/SDK** | 可直接复用 Pi 的 Agent/TUI 能力；工具生态丰富；构建迭代便利 | V8 堆、GC 与依赖运行时需要预算；系统操作仍需 driver；更适合作为可复用认知/执行组件候选 |
| **TypeScript + React + Ink** | React 组件化与生态；适合复杂终端交互组织 | 引入 React 的运行与渲染路径；当前没有必须复用 React 的需求；不能凭架构断言慢 |
| **Python + Textual** | 当前能力已实现并有测试；CLI/MCP/自动化生态成熟；可并发等待多个子进程 | 解释器与对象开销、原生分发需要考虑；不以迁移成本保留，不以 GIL 否定 I/O 并发 |
| **Swift 原生宿主** | macOS API 接入直接，未来原生桌面便利 | 首版终端与跨平台收益不足以成为本次首选；可保留为 macOS helper 候选 |

不再因“已有 Python”否决 Rust，也不把现有实现当作无价值：它是能力、语义和验证的复用基线。

### 4. 先复用，再补缺口

下表是实施约束，不代表 Rust 组件已经建成。迁移须保留已验证语义与测试场景；跨语言不能直接复用源码时，先检查薄适配能否满足需求，再决定移植范围。

| 拟建/调整的部分 | 已检查的能力 | 需要填补的缺口 / 最小动作 |
| --- | --- | --- |
| Rust Office 宿主 | `src/viva/` 成员、Task、Execution、grant、恢复、知识注册表 | 新宿主承载现有 Office 契约；保留身份、归属、授权、历史，不能用 Worker 会话替代它们 |
| Rust 执行监督 | `executions/runner.py`、Worker registry、CLI/MCP、Tokio process | 适配外部工具并落实有界输出、取消、进程组退出与回收；不重写模型循环 |
| Rust TUI | 当前 Textual surface、Ratatui/Crossterm、Pi TUI | 需要 Rust 宿主上的终端交互；复用渲染库，聊天/导航/日志与领域状态分离 |
| Git/worktree | `worktrees/service.py`、Git、`gh`、现有 authority | 保留隔离与受保护动作的规则；先复用命令与服务，不写 Git 引擎或 GitHub HTTP client |
| 脱敏与授权 | `core/redaction.py`、`permissions/authority.py`、grant ledger | Rust 路径必须覆盖原有拒绝、来源与脱敏行为；迁移不能扩大权限 |
| 计算机操作 driver | 浏览器自动化能力、macOS Accessibility/系统 API、现成 helper | 按实际任务验证后补最小适配；前台焦点/鼠标/键盘的动作序列须协调 |
| 存储演进 | 原子 JSON、append-only JSONL、Markdown | 只有事务/查询等实证缺口才触发新存储 ADR；先保留格式与导出能力 |

### 5. 状态与进程的所有权

Viva 拥有成员身份、Task、Execution 归属与状态、grant、workspace/project 关系及知识归属。模型与 Worker 可替换。外部工具可以拥有原始 transcript、模型消息格式及工具内部状态；Viva 保存稳定引用和必要交接记录，不同时维护两套互相竞争的会话事实。

Pi SDK/RPC 的选择尚未完成。采用 RPC 子进程时，Pi 内部会话事实属于 Pi；Viva 的 Office 事实仍属于 Viva。具体事件、取消、分叉、重连与持久化映射必须在接入前验证。[Pi RPC](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/rpc.md)、[Pi SDK](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/sdk.md)

一个 Office/TUI 可以监督多个按需启动的执行；16 个 Agent 不等于 16 个 Viva、16 个 TUI，也不保证可以共享为一个 Worker 进程。进程数量由各工具能力决定。

### 6. 关闭界面的语义

**批准的目标行为：关闭 TUI 后暂停任务派发与定期维护，不提供独立于界面的常驻 daemon。** “暂停”指保存未完成状态并停止继续推进，不承诺 OS 冻结整个进程或任意 CLI 能无损续跑。

正常退出应先停止新派发，持久化交接/执行状态，对 Viva 拥有的运行进程请求安全停止，等待并验证退出，必要时按策略升级终止。重新打开后先恢复事实，再由明确恢复动作或已授予的策略继续；无法确认的外部写入不得盲目重放。非 Viva 拥有的服务不因退出被误杀。

进程句柄丢弃、`kill_on_drop` 或 Rust 的内存安全不能替代进程树回收。崩溃/强制退出仍需要恢复协议。本行为尚未在当前 Python TUI 中完成验收。[Tokio Command](https://docs.rs/tokio/latest/tokio/process/struct.Command.html)

### 7. 用户故事带来的边界

- 协调成员的聊天、greeting/安静模式和谈话树，是 surface 与认知接入需求；谈话分叉不自动等于 Task 派发。成员名字不会固化为产品身份。
- Workspace 的项目、仓库路径与阶段性 reference 需要显式关系；参考 self-model/dsh-ai-soul 不产生运行依赖。
- 工作/短期/长期记忆及技能需要来源、取用、复审、失效与归档；不把全量历史塞入上下文，不把 raw event 自动标成 memory。自动机制仍属后续能力。
- 开发→验证→复核→交付可作为 Task 的可配置 workflow；不能复活 ticket/run/verdict 对象或固定流水线。PR/merge、删除代码/文档/worktree 等维护动作仍遵守授权边界。
- 定期维护只在 Viva 运行期间按策略执行。模型+工具组合须显式验证可用性，不静默换模型。
- 沙箱按动作风险与授权选择，避免一刀切；沙箱、凭证访问和操作权限是不同维度。复用已认证工具需要验证其实际运行环境，语言不能绕过 OS 或外部工具限制。grant 不因“不用沙箱”而扩大。

## Consequences and validation

### 资源与速度

宿主需采用有界队列与单条消息/总字节预算、日志落盘与轮转、按需载入历史、输出批处理和单一渲染者。并发槽位须考虑内存、CPU、I/O、浏览器、供应商限额；内存可能是主要约束，不能假设 CPU 永不受限。共享桌面的前台操作另行协调。

Rust 实现的首个真实任务垂直切片，同时承担技术验收。先记录目标 Intel Mac 的硬件/系统与空闲资源，再固定 Worker/模型/扩展/任务/输出负载，覆盖空闲、常态 1/少量执行及 16 路峰值：

| 验证项 | 必须留下的证据 |
| --- | --- |
| 内存 | 分开测 Viva、各 Worker 及其后代、浏览器与整机 memory pressure/swap；记录稳态与峰值，不能简单把共享页 RSS 相加当物理总量 |
| 交互与速度 | 冷启动、输入到绘制延迟 p50/p95、模型首输出、任务完成时间、CPU；区分本地与网络/模型耗时 |
| 输出与背压 | 高频输出下界面可响应、完整持久记录与可见截断策略、队列/日志不无限增长 |
| 生命周期 | 单独 Stop 不影响邻居；正常关闭停止所拥有执行；重启不丢归属、不重跑完成任务、不误杀其他进程 |
| 领域与授权 | 换模型保留成员历史；workspace/Task 独立；worker 请求不冒充用户；子 grant 不扩大；脱敏语义保留 |
| 工程反馈 | 冷构建、增量构建、检查/测试时间与缓存命中；记录 CI 总耗时，不把语言名当编译成本数据 |

量化预算需在实现切片前根据目标机器可用资源和用户响应要求确定，写进该切片的验收记录；本 ADR 不编造内存或毫秒门槛。语义验收要求零越权、零误杀、零归属丢失与零完成任务重放。性能预算未设或目标机器未测，不能报告“16 路低内存 Intel Mac 验收通过”。

若预算失败，先定位宿主、Worker、浏览器或调度的贡献；压缩外部负载或降低槽位可能比换语言有效。只有宿主被证明是主要瓶颈且替代栈在同负载达标，才据证据重新裁决。无需为了形式同时完整实现多套平台。

### CI 与迁移

Rust PR 需要检查/测试及必要构建，但不必每次丢弃缓存重编所有依赖。Cargo 支持构建缓存，`cargo check` 可提供不生成最终可执行文件的编译检查；两者都不替代测试。发布、平台/工具链或依赖变化可能需要额外构建，缓存也不能保证每次有效。[Cargo build cache](https://doc.rust-lang.org/cargo/reference/build-cache.html)、[cargo check](https://doc.rust-lang.org/cargo/commands/cargo-check.html)

本次不添加 Cargo 项目、不移植代码、不改变 Python 安装入口或 CI。实际迁移须单独交付可运行切片、行为证据、数据兼容/回退路径与目标平台验收；历史 Ticket Autopilot 数据保持原样。研究文档存档仅表示决策已收敛，不构成运行能力进度。

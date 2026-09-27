# Viva 技术选型的一手资料核查

核查日期：2026-09-27。范围为选型讨论，不构成迁移决定。

> **后续证据修正：** 用户进一步明确开发复杂度不参与选型，真实任务完成速度是核心指标。Pi 的 TypeScript、自有 TUI 与可复用 Agent 核心表明，不能仅依据 Rust 无 GC 就把它定为整体速度首选。最新核查见 [Pi 技术栈与资源设计](pi-coding-agent-stack-2026-09-27.md)；应优先验证 Pi 能力的复用，并以同负载数据比较资源和端到端速度。下文 Rust 首选是此前资源约束分析的阶段性判断，尚未经过此验证。

> **讨论前提已更新：** 用户明确要求忽略迁移成本，首版做终端 TUI，在较小内存的 Intel Mac 上支持 16 个并发 Agent；桌面与 Windows 只保留合理接口边界。本文件下方原有 Web/现有代码成本判断保留作资料背景，已不适合作为本轮推荐理由。以本节新增分析为准。

## TUI 与 16 并发的新判断

**推荐倾向（工程推论，非基准结果）：Rust + Tokio + Ratatui + Crossterm；Go + Bubble Tea 是接近的第二候选。** Rust 更贴近显式资源生命周期与避免 tracing GC 开销的优先级；Go 可以用更简洁的并发模型提供单一可执行文件与成熟 TUI。16 路主要为 I/O 的任务本身并不超过 TypeScript/Node 或 Python 的能力，也不足以证明 Rust 一定使用更少的实测内存。[Rust 所有权机制](https://doc.rust-lang.org/book/ch04-01-what-is-ownership.html)、[Bubble Tea](https://github.com/charmbracelet/bubbletea)

新增核实事实与限制：

- Rust 官方目前把 **Intel Mac 的 `x86_64-apple-darwin` 列为 Tier 2**，仍经 `rustup` 分发；官方记录 x86 最低 macOS 10.12。不能沿用“Intel Mac Tier 1”旧说法。目标可用不代表每个三方 crate 已在目标老机器完成验收。[Rust macOS 平台文档](https://doc.rust-lang.org/rustc/platform-support/apple-darwin.html)
- Tokio `mpsc::channel` 有限容量并提供背压；容量限制的是消息数量，因此还必须约束单条消息大小，才能形成字节预算。不能让非当前可见的 15 个 Agent 日志无限堆积。[Tokio mpsc](https://docs.rs/tokio/latest/tokio/sync/mpsc/index.html)
- Tokio 子进程句柄丢弃默认不杀进程；`kill_on_drop` 默认 false，即使打开也只有尽力回收保证。需要显式 `wait().await` 或 `kill().await`；Unix `process_group(0)` 可为子进程建立独立组，但发送组信号、拒绝误杀与退出确认仍需 Controller 实现。[Tokio Command](https://docs.rs/tokio/latest/tokio/process/struct.Command.html)
- Ratatui 默认支持 Crossterm backend，同时有 `TestBackend`；终端绘制和键鼠事件由 backend 提供。应保持单一渲染者，16 个 Agent 向它提交状态与事件，而非 16 个 TUI 或 Controller 实例。[Ratatui backends](https://ratatui.rs/concepts/backends/)
- Go GC 可以用 `GOGC` 权衡 CPU 与堆占用，也支持 `GOMEMLIMIT`。后者是软限制，运行时可能超过；不是进程树或整机内存硬限制。官方也提示对输入与环境不受控的 CLI/桌面程序，不宜盲目内置该限制。[Go GC guide](https://go.dev/doc/gc-guide)

| 候选 | 在新前提下的判断 |
| --- | --- |
| Rust + Tokio + Ratatui/Crossterm | 资源管理优先的首选；没有 GC 不等于不会泄漏/无限缓存，异步生命周期与进程回收要显式验收 |
| Go + Bubble Tea | 并发、单二进制、TUI 与开发复杂度的强平衡；有 GC，但不能未经实测断言老 Mac 上不合格 |
| TypeScript/Node + Ink | React 交互迭代便利，16 路 I/O 可实现；当前没有必须复用 React 的需求，不能拿未来桌面界面作为首版引入理由 |
| Python + Textual | 可以实现，异步 Worker 与 TUI 具备现成能力；当低控制器开销优先且不计迁移成本时，优先级下降；不是因 GIL 就无法同时等待 16 个子进程 |

验收应同时量 **Viva 本身**与 **16 个真实 Agent 及其后代总量**：常驻/峰值物理内存、整机 memory pressure/swap、CPU、输入响应、输出丢失、Stop 回收结果。不能写一个 Rust Controller 就承诺老 Mac 能跑 16 个真实 Claude/Codex CLI；大头可能来自外部 CLI，与 Controller 语言不同。

最小实现边界：一个 Controller，一个 TUI，16 个受预算约束的 Agent 会话；日志落盘、可见窗口保留有界缓存，状态更新合并，队列与消息大小有上限。只隔离核心状态/命令/事件、Agent adapter、终端 UI、平台进程控制四处职责，不为未来桌面端新增服务或跨进程协议。

## 问题边界

`IDEA.md` 定义的是 Ticket 驱动的自动软件交付；`docs/closed-loop-workflow.md` 当前定义的是 Plane-first、localhost Web 服务，调用外部 Developer/QA CLI，保存检查与审计证据。应分别选 Controller 语言、用户界面、持久化与外部 Agent 协议，不能拿一个终端产品的 UI 技术决定所有层。

## 已核实事实

- **Ink 是终端 React renderer。** 官方 README 将其定义为 interactive command-line apps；它有组件测试工具，并区分 CI 渲染行为。它能复用 React 的编程模型，不意味着终端组件可以原样成为浏览器 DOM 组件。对于当前 Web 产品，Ink 本身没有直接贡献。[Ink 官方仓库](https://github.com/vadimdemedes/ink)
- **Claude Code 的发布方式不能简单归为 Node 工具。** 当前官方 setup 推荐 Native Install；明确说明 npm 从 v2.1.198 起安装与独立安装器相同的 native binary，运行的 `claude` binary 不调用 Node。该文档没有证明其全部源码语言、UI 内部实现；本次没有找到足以独立确认完整 TypeScript + React + Ink 架构的官方材料，因此将用户给出的源码栈视作待核实前提，而非本项目决策依据。[Claude Code 官方 setup](https://code.claude.com/docs/en/setup#install-with-npm)
- **Python 能处理异步子进程，但没有自动解决进程生命周期。** Python 3.11 提供 `asyncio.create_subprocess_exec`、stdout/stderr 流、terminate/kill；`wait` 本身没有 timeout 参数，需配合 `wait_for`。只等待不消费管道可死锁，`communicate` 将输出缓存在内存，不适合无限输出。POSIX 与 Windows 的终止语义不同。[Python 3.11 subprocess 文档](https://docs.python.org/3.11/library/asyncio-subprocess.html)
- **Node 能处理异步子进程，取消也不等于清理进程树。** `spawn` 不阻塞事件循环，支持 `AbortSignal`；输出管道容量有限。官方明确指出 Linux 下 kill 父进程不会终止其子进程；`killed` 仅表示信号成功发出。detached 的平台行为也不同。[Node child_process 官方文档](https://nodejs.org/api/child_process.html)
- **Go 可编译为可执行文件，可把 Web 静态资源嵌入。** `go build` 生成 executable，`embed` 可以在编译时纳入文件。由此可以设计成一个 Viva 可执行文件携带浏览器界面；这是方案推论，仍需按平台构建、签名、发布，仍依赖 Git 与外部 Agent CLI，也不能保证所有 cgo/原生依赖都消失。[Go 编译教程](https://go.dev/doc/tutorial/compile-install)、[embed 官方文档](https://pkg.go.dev/embed)
- **Go 取消语义仍需自己设计。** `CommandContext` 默认 Cancel 调用该 Process 的 Kill；默认没有设置 WaitDelay。可以定制 Cancel，设置 WaitDelay 来限制未退出进程或未关闭管道的等待。官方没有承诺默认清理整个进程树；平台专属配置仍是 Controller 的职责。[Go os/exec 官方文档](https://pkg.go.dev/os/exec#CommandContext)
- **静态类型与外部证据校验是两件事。** Python 的注解不会被运行时强制执行，需配置类型检查器。TypeScript 类型注解在编译后被擦除；其 discriminated unions 可以帮助表达状态与穷尽分支，但不能自动校验来自 Plane、Agent CLI、磁盘文件的 JSON。[Python typing](https://docs.python.org/3.11/library/typing.html)、[TypeScript Basics](https://www.typescriptlang.org/docs/handbook/2/basic-types.html)、[TypeScript Narrowing](https://www.typescriptlang.org/docs/handbook/2/narrowing.html)
- **Python 与 Go 也有完整 TUI 方案。** Textual 提供异步/线程 Worker，与 UI 生命周期协调；Bubble Tea 使用 Model/Update/View 的 Elm 式事件模型。因此“想要 TUI”不足以推出必须改成 TypeScript。[Textual Workers](https://textual.textualize.io/guide/workers/)、[Bubble Tea 官方仓库](https://github.com/charmbracelet/bubbletea)
- **Rust 有相关行业例子，但理由不能直接移植。** OpenAI 的 2025 年 Codex CLI 作者说明从 TypeScript/Ink 改到 Rust 的诉求是免 Node 安装、原生 sandbox bindings、内存/性能和可扩展 wire protocol。这证明 UI 和核心可以分层选择；Viva 当前委托现成 Agent 执行任务，尚不能推导出需要重建原生安全 harness。[OpenAI 原作者公告](https://github.com/openai/codex/discussions/1174)

## 由事实得到的选型判断（推论）

| 候选 | 最有价值的适用条件 | 当前不能由其解决的问题 |
| --- | --- | --- |
| Python Controller + 浏览器 UI | 已有代码与验证资产；核心工作是集成、检查、证据处理；先把一张真实 Ticket 闭环 | 类型纪律、重启恢复、授权校验、持久化原子性与进程树控制仍要明确设计 |
| TypeScript/Node + React 浏览器 UI | 产品主要复杂度在交互；希望前后端共享 schema/事件定义；团队熟悉 TS 或深度依赖 JS SDK | 不能靠静态类型校验外部 JSON；不自带可靠任务队列或进程树终止；Ink 对 Web 无帮助 |
| Go Controller + TypeScript 浏览器 UI | 目标是面向多人分发的本地控制器；安装摩擦、长驻进程、取消与并发可维护性成为可测量瓶颈 | 需要承担双语言契约与发布链成本；外部 Agent CLI 的安装与鉴权不会消失 |
| Rust Controller + 浏览器 UI | 确认需要原生沙箱/系统级隔离、严格资源上限或原生核心被多个客户端复用 | 当前没有证明这些是 Viva 最紧迫的缺口；重写风险不能由 Codex 的选择抵消 |

这不是语言性能排名。当前 Controller 的主要等待发生在网络、Git、检查和外部 Agent，是否有语言运行时瓶颈必须以测量证明。

## 更值得作为淘汰标准的验证

1. 外部 CLI 大量输出、stdout/stderr 同时写入时 UI 仍响应，证据不丢失且内存受限。
2. Stop 在子进程树、忽略 TERM、管道被后代保持打开时都有明确结果，并不会信号误发到其他运行。
3. Controller 在 QA 或 Commit 边界崩溃后，恢复的是可信证据状态，不重复执行不可幂等操作，也不假造 QA PASS。
4. 外部非法 JSON、状态与 attempt 不匹配、重复 Owner action 均被运行时契约拒绝。
5. 在声明支持的操作系统上，从安装到一张真实低风险 Ticket 的完整运行有可复现证据。

这些标准同时适用于四种语言。迁移只有在修复现有实现比迁移更昂贵，或产品分发/系统约束明确变化时才有充分理由。

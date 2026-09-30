# Herdr 架构与功能映射研究：Viva 终端运行时的对齐、冲突与裁决点

Status: planning evidence; official docs/API/source metadata inspected, no herdr source line-by-line audit, nothing built or benchmarked. Date: 2026-10-01.

[Goal check] This work advances Viva's runtime capability by mapping herdr's full feature surface onto Viva's user stories and naming the exact ADR conflicts and decision points a "herdr-like Viva" requires.

## 1. 定位、固定来源与许可

用户的问题是：能否让 `viva` 启动后"像 herdr 一样"——在 TUI 里展示 worktree 和工作中的 agent，且不需要插件、现有 herdr 能实现的都要实现。本文回答三个前置问题：herdr 到底有什么；每一项映射到 Viva 的什么现状与缺口；哪些项目与已接受裁决冲突、必须先裁决。它是 [Orca 复用审计](orca-reuse-audit-2026-09-28.md) 的同族后续：同一"多 worktree 并行 agent 工作面"目标，herdr 是其中终端复用器形态的成熟参照物。

审计对象是开源项目 herdr（"the runtime your coding agents live on"），v0.9.3。来源固定如下，本文全部引用以该状态为准：

- 仓库 [herdrdev/herdr](https://github.com/herdrdev/herdr)，master HEAD `347f9c99bc95672e5ebcb26753648e85801d3c71`（2026-09-30）。官方 [agent guide](https://herdr.dev/agent-guide.md) 引用的 [ogulcancelik/herdr](https://github.com/ogulcancelik/herdr) 经 GitHub API 实时核实是该仓库转移前的旧地址，现重定向到 `herdrdev/herdr`。
- 许可 **Apache-2.0**（API 实时读取）。允许复用与修改；若复制其代码或实质性材料，须保留其许可与版权声明，Viva 的许可不代替这些声明。约 41.6k stars。
- 规模（GitHub API 实测）：第一方 `src/` 为 **376 个 .rs 文件、约 9.75 MB**（粗算 25–30 万行）；另有 vendored 的 Ghostty VT 解析 crate（`crates/ghostty-vt`）与打了补丁的 `vendor/portable-pty`。作为对照，Viva 现有 `crates/viva/src` 为 36 个文件、约 816 KB。
- 技术栈（Cargo.toml 实读）：ratatui 0.30 + crossterm 0.29 + tokio + portable-pty + 自带 VT 仿真 + interprocess 本地 socket + clap。与 [ADR 0011](../decisions/0011-rust-host-and-tui.md) 记录的实施默认组合同栈。

## 2. Herdr 功能全景（以官方文档为准）

概念模型：**session（后台 server 上的命名空间）→ workspace（项目容器）→ tab（布局单元）→ pane（真实终端）**。客户端可随时 detach/attach；进程活在 server 里，不活在客户端里。

| 功能域 | 具体能力 |
| --- | --- |
| 复用器核心 | 后台 server + 命名 session；workspace/tab/pane 树；鼠标优先（点选、拖边、右键菜单）；tmux 兼容 prefix（ctrl+b）；navigate/copy/resize 模式；pane zoom |
| Agent 感知 | 识别约 20+ 种 agent CLI（Claude Code、Codex、Copilot、Cursor、Gemini、opencode、Crush、Qwen 等）。状态 working/blocked/done/idle/unknown；sidebar 按 pane→tab→workspace 汇总。检测三层：前台进程树识别 → 屏幕清单（对底部缓冲快照跑 TOML 规则，最后手段）→ **per-agent lifecycle hook 集成（权威状态）**。官方文档明说屏幕检测会误报，hook 集成才是可靠来源 |
| Agent 操作 | `agent explain/wait/prompt`（发送并等待）、`pane attach` 直连、自定义标签与 metadata token、远程 manifest 更新；`agent wait` 是 server 持有的事件，客户端退出后仍存活 |
| 会话状态 | detach/reattach 进程不停；server 重启后恢复布局形状 + 可选 pane 历史回放 + 经集成恢复 agent 原生会话（记录 resume 命令）；live handoff（更新/远程连接时转移活 PTY，实验特性）；无头固定尺寸终端供编排 |
| Worktree | `worktree.create/open/remove`：在配置目录批量建 worktree、以分组 workspace 呈现在 sidebar 的 Git 区、移除先安全后强制；worktree 动作挂在 Git workspace 行上 |
| Socket API | NDJSON over Unix socket/Windows 命名管道；`herdr api call` 与交互 REPL；方法组覆盖 server/notification/client/session/workspace/worktree/tab/pane/popup/layout/agent/events.subscribe/integrations/plugins；`session.snapshot` 提供全量引导快照；带协议版本 |
| 插件 | manifest 化的可执行工作流插件（动作、事件钩子、自定义 pane、链接处理）+ GitHub 分发。**用户已明确 Viva 不需要** |
| 多机 | `--remote` 瘦客户端；本机与保存的 SSH 机器同窗、agent 列表合并、各自独立重连；SSH 保活与 control socket 复用 |
| 配置面 | 键位、主题（内置 + 自定义 TOML + 明暗自动切换）、UI（sidebar 行/宽度、pane 边框、tab 栏、状态区、窗口标题）、toast 通知与音效（按 agent 配 mp3）、scrollback、终端默认（shell/login/cwd 策略）、实验项（pane 历史、kitty 图形协议、CJK 输入法光标跟随）、环境变量族（HERDR_SESSION/SOCKET/ENV/THEME…）、日志轮转 |
| 其他 | onboarding、自更新（更新时 live handoff）、嵌套启动阻断（HERDR_ENV）、给 agent 用的 SKILL.md、Windows 支持含 WSL 说明 |

## 3. 逐项映射到 Viva

| Herdr 能力 | Viva 现状 | 缺口量级 | 与既定裁决的关系 |
| --- | --- | --- | --- |
| 后台 server + detach/attach | 无。`viva` 是单进程宿主 | 大 | **与 [ADR 0011 §6](../decisions/0011-rust-host-and-tui.md) 直接冲突**：已裁决"关闭 TUI 后暂停派发与维护，不提供独立于界面的常驻 daemon"。要 detach 必须先修订该 ADR，这是用户裁决项 |
| workspace/tab/pane 布局引擎（鼠标、prefix、zoom） | [tui/](../../crates/viva/src/tui/mod.rs) 有导航外壳 + 单焦点 pane 投影；无布局树/鼠标/模式机 | 中 | 无冲突；ratatui 能力范围内的新增 |
| 终端仿真 + 有界 scrollback | [terminal/](../../crates/viva/src/terminal/mod.rs) 已有 vt100 解析 + 2000 行上限 + 全保真脱敏磁盘日志（V05, issue #14） | 小（可选换/对齐 herdr 的 ghostty-vt） | 无冲突；ADR 0011 §5.3 已预留"具体库待实现验证" |
| PTY 生命周期与进程组回收 | [terminal/session.rs](../../crates/viva/src/terminal/session.rs) 已有 SIGTERM→升级 SIGKILL→确认 reap 的全流程 | 基本对齐 | 无冲突 |
| Agent CLI 识别与五态展示 | 无 | 大（20+ CLI 的规则/钩子是持续维护的产品面） | **方向同构**：herdr 自己也发现屏幕检测不可靠、改用集成钩子拿权威状态——这正是 ADR 0011 §5.2"终端输出只是原始记录，不能靠 ANSI 猜权威结论"的 Viva 版本。Viva 可做"屏幕推断仅作展示、权威状态走受控接口"的双层 |
| Worktree 展示与 create/open/remove | [git/](../../crates/viva/src/git/) worktree service + TUI 导航已有 | 小到中（缺"worktree 分组 workspace"式 UI 汇总） | 无冲突；注意 charter 的 worktree 生命周期人工管控规则不因 UI 好用而放宽 |
| Socket API + 事件订阅 + agent wait/prompt | 无独立 socket 面（有 office CLI） | 大 | ADR 0011 §5.4 把 RPC/SDK 留作"未来可选"；若做，是新增裁决而非既有路线 |
| 插件/市场 | 无 | — | 用户已排除，无需动作 |
| SSH 多机同窗 | 无 | 大 | 本地优先之外的能力，ADR 无此项；属范围裁剪裁决 |
| 主题/音效/通知/kitty 图片/CJK IME 实验项 | 无 | 长尾 | 非北极星必需；进入与否属范围裁剪 |
| 自更新/安装器 | npm 分发已有（PR #39） | 小 | 对齐自身路线即可，不需要照搬 |
| 无头固定尺寸终端供编排 | terminal spec 已可固定 cols/rows | 小 | 无冲突 |

结论：**"启动后像 herdr"的体验骨架（多 pane + worktree 行 + agent 状态）对 Viva 是"补强既有种子"，而"全部功能对等"里有三块（常驻 daemon、socket 编排面、20+ agent 检测矩阵）每一块都接近或超过 Viva 现有整个宿主的体量，且第一块与已接受 ADR 相反。**

## 4. 三条路线与代价

延续 [Orca 审计 §5](orca-reuse-audit-2026-09-28.md) 的裁决框架，按宪章 Architecture Order（先复用、后最小 glue、再建新组件）：

- **路线 A：原生长大（ADR 0011 现行路线）**。不动 daemon 裁决，把 terminal/tui 长成多 pane 网格 + worktree 分组展示 + agent 状态双层（屏幕推断仅展示、受控接口为权威）。估计为"数月量级的持续切片"，不是数年；每个切片可独立验收。
- **路线 B：包一层 herdr**。Viva 做控制面（成员/任务/授权/记录不变），经 herdr 的 CLI/socket API 把它当作被监督的终端运行时；最短路径获得完整 herdr 体验。代价：必须修订 ADR 0011 §6 的无常驻 daemon 裁决（herdr server 常驻是其前提）；引入第三方二进制运行时依赖（版本固定、Apache-2.0 notice、升级受 herdr 节奏约束）；且经 ANSI 屏幕拿到的 agent 状态仍不能当 Viva 的权威证据。
- **路线 C：fork/vendor herdr 源码**。Apache-2.0 允许；能力最全。代价：从此维护约 30 万行外部快速迭代代码，与宪章"不为保留而保留、按缺口最小实现"相反，长期成本三路线最高。

本文不替用户选路线；第 5 节列出必须由 Haisu 裁决的问题。

## 5. 需要 Haisu 裁决的问题

1. **是否修订 ADR 0011 §6**，允许常驻运行时（detach/attach、跨重启恢复）？不修订则 herdr 的灵魂功能整体出局，路线 A 的"像 herdr"只能限定在"前台运行期间的多 pane 体验"。
2. **范围裁剪**：SSH 多机、插件（已排除）、主题音效、图片渲染是否属于"都要实现"？建议明确排除并把"都要"锚定到 §3 表格的功能域。
3. **路线选择**：A / B / C（或 A 先行、B 作为过渡验证）。
4. **agent 状态的定位**：无论哪条路线，屏幕推断状态只能做展示层；验收、派发与恢复继续走受控接口。建议写进对应切片的验收记录。
5. **若走 B**：第三方二进制依赖政策（固定版本、许可 notice、其升级节奏与 Viva 发行的关系）需要一条新的小裁决。

## 6. 已验证与未验证

已验证：herdr 官方站点与 docs 的功能面（2026-10-01 实时抓取：agent guide、agents、session-state、configuration、socket-api）；GitHub API 元数据（Apache-2.0、约 41.6k stars、Rust 主导、HEAD `347f9c9` 固定）；`src/` 与 vendored 规模实测；Cargo.toml 依赖栈实读；Viva 侧 terminal/tui/git 模块现状（源码头注释实读）与 ADR 0011 全文。

未验证：herdr 源码逐行审计（本文止于官方文档 + Cargo.toml + API 元数据层，未读其 pane/检测/恢复的具体实现，也未本地运行 herdr）；官方文档与二进制实际行为的逐项对照；路线 A/B/C 的成本估算为研究级判断，非实测数据；herdr 检测矩阵对具体 agent 版本的实际准确率；其 vendored ghostty-vt 与 Viva 现用 vt100 crate 的行为差异。研究工件不构成实现、验收 PASS 或性能证据。

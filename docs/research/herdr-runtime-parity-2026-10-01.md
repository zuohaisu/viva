# Herdr 架构与功能映射研究：Viva 终端运行时的对齐、冲突与裁决点

Status: planning evidence; official docs/API/source metadata inspected, no herdr source line-by-line audit, nothing built or benchmarked. Route decision: **A 原生实现**（2026-10-01，Haisu 裁决，见 §5/§6）；同日二轮裁定反转 daemon 范围（见 §5.1），ADR 0011 §6 修订文本待剩余裁定收齐后提出. Date: 2026-10-01.

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

本文不替用户选路线；第 5 节列出必须由 Haisu 裁决的问题（2026-10-01 已裁决，见 §5）。

## 5. Haisu 的裁决（2026-10-01 记录）

2026-10-01，Haisu 对 §4 的路线选择裁决：**路线 A——原生实现，读 herdr 学思路，自己实现**。该裁决同时给出其余各问的答案，均为路线 A 定义的直接推论；若后续推翻应在此追加记录，不回写历史：

1. **ADR 0011 §6 不修订**：无常驻 daemon 的裁决保持。herdr 的 detach/attach 与跨重启活恢复不在路线 A 范围内；"像 herdr"限定为前台运行期间的多 pane 体验，加上 §6 语义的重启恢复（恢复事实与归属，恢复动作显式触发）。
2. **范围锚定**："都要实现"锚定到 §3 表中与 Viva 北极星相关的功能域：多 pane 布局、worktree 分组展示、agent 状态展示层、无头固定尺寸终端。SSH 多机、插件、主题音效、图片渲染、自更新通道按长尾排除，不进切片。
3. **路线**：A（本条即裁决本身）。
4. **agent 状态定位**：屏幕推断仅作展示层；验收、派发与恢复继续走受控接口（ADR 0011 §5.2），写进对应切片的验收记录。
5. **第三方二进制依赖政策**：随路线 B 出局，不适用。

### 5.1 二轮裁定（2026-10-01，逐项过审）

同日 Haisu 逐项复审功能面，**路线 A（原生实现）不变**，但第一轮第 1、2 条中被本轮裁定取代的子项按"追加不回写"规则记录如下：

已裁定：

1. **常驻 server + client/server 拆分：要**（detach/attach 的前提）。→ ADR 0011 §6 需修订，修订文本待本轮全部裁定收齐后随本文档一并提出；§6 切片序列同步重排，server/client 拆分将成为新的早期切片。
2. **detach/attach：要。**
3. **live handoff（活 PTY 转移）：要**（依赖常驻 server）。
4. **SSH 多机同窗：后续阶段必须，首版不做**（建立在常驻 server 之上）。
5. **音效与 toast 通知：要。**
6. **主题系统：首版不做，后续加。UI 国际化：首版不做，后续加。**
7. **agent 检测首版范围：codex、claude、codebuddy、qodercli、cline、hermes、pi coding agent，共 7 个**；其余按"需要哪个加哪个"。注：herdr 现有规则表可见 claude/codex/cline，codebuddy/qodercli/hermes 大概率需自建规则（自建同时避免 §7 的实质性材料复制问题）；pi 在 Viva 内可经受控扩展取得权威状态，优先于屏幕推断。
8. **不要：插件与市场、onboarding 向导、面向 agent 的 SKILL.md。键位自定义：暂不需要。**

待裁定（已请求解释，解释后由 Haisu 裁定）：

- 跨重启"活恢复"（进程未死过的类型二恢复）与 live handoff 的工程边界
- 屏幕推断的定位（权威状态 vs 仅展示层）
- kitty 图片协议渲染
- socket API 终端原语编排面
- CJK 输入法光标跟随
- 常驻 server 回归后，关闭 TUI 时任务派发与定期维护"继续还是暂停"（§6 原文为暂停，daemon 语义下两者皆可行）

## 6. 路线 A 的执行含义与切片序列

路线 A 的证据基线：herdr 的价值是"哪些能力组合成立"的活样本，不是代码来源。Viva 带走的是思路——session→workspace→tab→pane 的导航树、五态 agent 展示、检测分层的谨慎（进程树识别优先、屏幕规则兜底、集成钩子权威）、按功能域拆配置——全部可不经其代码独立实现；实现落在 Viva 既有领域模型（成员/任务/执行/授权）之上，不照搬 herdr 的 pane 中心模型。

切片序列（建议作为 [V14 issue #27](https://github.com/zuohaisu/viva/issues/27) 的子问题拆分，与已关闭的 V05/V06/V07/V08 能力衔接）：

1. **多 pane 布局引擎**（`tui/`）：pane 树（分割/焦点切换/zoom），多终端快照同屏的绘制预算（V06 目前单焦点 pane）。验收：≥3 个真实终端 + 1 个 shell，切换、resize、邻居互不影响。
2. **worktree 分组工作台**（`git/` + `tui/`）：V08 的 worktree 服务以分组行呈现，行上挂 create/open 动作；remove 仍走人工授权，charter 的 worktree 生命周期规则不因 UI 好用而放宽。验收：对真实仓库展示 ≥3 个 worktree 并可进入。
3. **agent 状态展示层**（`terminal/` + `tui/`）：第一层做前台进程树识别（herdr 同思路的 Rust 最小实现）；屏幕规则仅作有界实验并在 UI 标注 unverified。验收：对 1–2 个真实 agent CLI 展示 working/blocked/done，且与受控接口记录的执行状态并列呈现、不互相冒充。
4. **无头固定尺寸终端**（`terminal/`）：供编排使用的固定 cols/rows 会话，快照可编程读取。验收：无 UI 也能创建、读快照、按策略停止。
5. **重启恢复（ADR 0011 §6 语义）**：重启后恢复事实与归属，恢复动作显式触发（Pi 原生 resume 属 V09/V10 交界，不在此片承诺）。验收：重启不丢归属、不重跑已完成任务。

每一片走标准 worktree→PR 流程；V12 的 16 路并发资源门槛不变——多 pane 不等于 16 个常驻渲染，绘制仍走 V06 的"缓存快照、单渲染者"纪律。

## 7. 许可与归属合规

路线 A 的合规基线是零复制：读 herdr 学思路、自己实现，不触发 Apache-2.0 的任何声明义务（permissive 许可也不要求 clean-room 流程）。红线沿用 [Orca 审计已立规则](orca-reuse-audit-2026-09-28.md)：实际复制代码及实质性材料时须保留其版权与许可声明，另行保留第三方许可，Viva 的 MIT 不代替这些声明。

- 若未来任何切片出现逐字/近似逐字复制（代码、注释、测试用例、规则表结构）：该 PR 必须同时引入 `THIRD_PARTY_NOTICES.md`（组件、版本/commit、许可证、来源链接）并在 README 加致谢节。herdr 无 NOTICE 文件（2026-10-01 核实），NOTICE 随附义务为空。
- 若引用其检测规则表（`distribution/agent-detection/*.toml`）的实质内容，视为实质性材料复制，按上一条处理；自建规则表配自测样例不触发。
- `@zuohaisu/viva` npm 分发若将来捆绑任何第三方二进制，发行包内须随附对应许可文本。
- 不以 "herdr" 名称或商标为 Viva 背书（Apache-2.0 §6）；本文档这类事实性来源说明不受限。

## 8. 已验证与未验证

已验证：herdr 官方站点与 docs 的功能面（2026-10-01 实时抓取：agent guide、agents、session-state、configuration、socket-api）；GitHub API 元数据（Apache-2.0、约 41.6k stars、Rust 主导、HEAD `347f9c9` 固定）；`src/` 与 vendored 规模实测；Cargo.toml 依赖栈实读；Viva 侧 terminal/tui/git 模块现状（源码头注释实读）与 ADR 0011 全文。

未验证：herdr 源码逐行审计（本文止于官方文档 + Cargo.toml + API 元数据层，未读其 pane/检测/恢复的具体实现，也未本地运行 herdr）；官方文档与二进制实际行为的逐项对照；路线 A/B/C 的成本估算为研究级判断，非实测数据；§6 切片序列为建议，未经 issue 拆分、排期或任何实现；herdr 检测矩阵对具体 agent 版本的实际准确率；其 vendored ghostty-vt 与 Viva 现用 vt100 crate 的行为差异。研究工件不构成实现、验收 PASS 或性能证据。

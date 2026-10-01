# V15 规划 — 工作台深化：从"herdr 对齐"到"日常主力"

Status: planning（Haisu 2026-10-02 口述五缺口，本文转化为切片与工单；无任何实现）。Date: 2026-10-02.

[Goal check] This work advances Viva's runtime/workbench capability by closing the five gaps a real one-day herdr trial exposed, each slice with acceptance evidence defined before implementation.

## 1. 背景：一天 herdr 实测的结论

V14（PR #50，#43–#48）交付了 herdr 形态的运行时：常驻 server、detach、多 pane、活恢复、三源 agent 状态、编排 API、暂停语义。2026-10-01/02 Haisu 用 herdr 做了一天真实工作，结论：**herdr 仍无法取代 Orca 的若干日常优点**。这不是 V14 方向的否定——Viva 的工作台底盘（成员/任务/授权/审计）正是 herdr 没有的——而是下一阶段的输入：五个缺口，全部落在"工作台的信息可见性与自动化"上。

用户原话归纳的五个缺口：

1. Orca 在每个 worktree 能看到工作区文件，点开能看文件内容与修改；
2. Orca 清楚 worktree 的 PR 是否已合并，合并后提供删除 worktree 的入口；
3. Orca 提供不同 agent 之间的 handoff；
4. Orca 自动做 git pull（herdr 需要每次人工）；
5. （新增需求）遇到 5 小时/数小时的用量限额，只要知道几点恢复，就等到时间自动恢复工作。

## 2. 切片总览

| 切片 | 一句话 | 依赖 | 主要触及 |
| --- | --- | --- | --- |
| V15-1 文件浏览器与差异查看 | worktree 里看文件、看单个文件改了什么 | 无 | `tui/`（新文件面板）、`git/cli`（ls-files/diff 单文件） |
| V15-2 PR 状态与合并后清理 | 行上见 PR 状态；merged 后一键清理（人工确认） | 无（需 gh） | `git/`（新 pr.rs）、workbench 行、V08 release |
| V15-3 跨 agent 交接 | 把一个 worktree 从 agent A 手递给 B，带 brief | 无（软依赖 V15-1 的选择 UI 经验） | workbench 动作、task brief、handoff 记录 |
| V15-4 自动 git 同步 | 主 checkout 自动 ff-only pull；worktree 自动 fetch + behind 提示 | 需 §4 裁决确认 | `git/worktrees`（政策修订）、maintenance 挂接 |
| V15-5 限额定时恢复 | 限额 + reset_at → 到点自动恢复，跨重启 | 无（建于 S4/S6 之上） | `agents/`（申报字段）、server 调度、settings |

每片独立 worktree→PR→验收，与 V14 切片同纪律。

## 3. 切片定义

### V15-1 文件浏览器与差异查看

What：worktree 行上 `f` 进入该 worktree 的文件面板：`git ls-files` + status 标记（M/A/D/未跟踪）分组的文件树；在文件上回车 = 内容视图（有界读取）或 unified diff（对该 worktree 的 `base_sha`，即 V08 记录的基线）。`e` 用 `$EDITOR` 打开当前文件（编辑仍属用户工具，Viva 不建编辑器）。

验收（草拟，入 issue）：
- [ ] ≥3 个真实 worktree 各自可浏览文件列表，修改/新增/未跟踪有区分标记
- [ ] 进入单个修改文件显示对 base_sha 的 unified diff；未修改文件显示有界内容
- [ ] 大文件/二进制有界并如实标注（复用 64 KiB bound 模式），不假装完整
- [ ] 面板只读；`e` 调起用户编辑器成功；Esc 返回不丢 pane 布局
- [ ] 与 pane 布局共存（文件面板可作为 browser pane 的一种视图，不破坏 S2 布局持久化）

### V15-2 PR 状态与合并后清理入口

What：worktree 行显示其分支关联的 PR 状态（open / merged / closed / none，checks 摘要），数据来自 `gh`（复用成熟能力，宪章顺序 1）；merged 时行上出现清理入口 `D`：确认对话框**点名确切路径**后执行——先 V08 release（记录），再 `git worktree remove`；失败如实报告。绝不自动删除；删除即人工授权（点击+确认命名目标），记入审计。

边界：gh 不可用/无 PR 时如实显示 none，不猜；closed-not-merged 不提供清理入口（那是人的判断）。

验收：
- [ ] 真实仓库 + 真实 PR：open 与 merged 两态可见且正确（gh 实测）
- [ ] merged 分支的 worktree 可经 `D` → 确认（显示确切路径）→ release + remove 成功，动作留审计
- [ ] 确认前取消无任何副作用；非 merged 状态无清理入口
- [ ] 无 gh/无网络时降级显示，不阻塞工作台

（注：PR/checks 查看本就是 V14 #27 交付范围的一句，本片正式收口该遗留。）

### V15-3 跨 agent 交接

What：worktree 上 `h` 触发交接：选目标 agent（已装 CLI 列表）→ Viva 汇编 brief（任务目标与约束 + 离场 agent 的最后 handoff 摘要〔来自 S4 受控申报〕+ worktree/branch 上下文）→ 在同一 worktree 启动目标 agent 终端并投递 brief → 交接事件记入任务历史。原终端不自动杀（人决定）。

边界：brief 是材料传递，不承诺上下文无损（ADR 0011 §5.4 原则）；离场摘要缺失时 brief 如实标注"无离场摘要"；被交接对象不自动获得授权（grant 语义不变）。

验收：
- [ ] 真实流程：agent A（可用 `cat` 假扮）→ `h` → 选 B → B 终端收到含任务目标与 A 摘要的 brief
- [ ] 任务历史出现交接记录（谁→谁、brief 摘要、worktree）
- [ ] 无离场摘要时标注，不编造；A 的终端保持运行由人处理

### V15-4 自动 git 同步（2026-10-02 Haisu 裁决：通过，附设置项）

What：**主 checkout**（project.repo_path 指向的检出）自动同步：仅 `--ff-only`、仅干净树。**设置项 `auto_pull`（默认关闭）**：由用户显式开启/关闭（CLI 与工作台均可切换），持久化于 office_settings（与 pause 同表，跨重启）；关闭时完全不执行自动 pull（自动 fetch 与 behind 提示不受影响）。工作台打开时与维护周期各同步一次（受 S6 pause 门控）；每次同步记 office_events。**任务 worktree 永不自动合并**——只自动 `fetch` 并在行上显示 behind/ahead，是否重排由人决定（保持任务隔离与 V05"不误杀别人工作"精神）。

已裁决边界（2026-10-02，Haisu）：
> 主 checkout 允许自动 `git pull --ff-only`（仅干净树）；任务 worktree 维持 fetch-only 永不自动合并；一切自动同步动作留审计；pause 门控适用。另设 auto_pull 开关，默认关闭，人工开启后方可自动 pull。

验收：
- [ ] auto_pull 关闭（默认）：主 checkout 无任何自动 pull；fetch/behind 提示照常
- [ ] auto_pull 开启：主 checkout 落后时自动 ff 前进并记录；脏树/非 ff 跳过并如实提示，绝不 force/merge
- [ ] 开关经 CLI 与工作台均可切换并持久化（重启后保持）
- [ ] 任务 worktree 仅 fetch + behind 显示，工作区不被自动改动
- [ ] pause 状态下不自动同步；恢复后下一周期补上
- [ ] 网络失败静默降级为"上次同步时间"显示

### V15-5 限额定时恢复

What：把"知道几点恢复就到点自动恢复"建成一等能力。链路：成员经 S4 受控申报 `AgentReport` 扩展 `rate_limited` 状态 + `reset_at`（RFC3339；用户也可在 UI/CLI 直接设定）→ server 持久化唤醒计划（SQLite：task/terminal、reset_at、恢复动作）→ 常驻 server 定时器到点执行恢复（重发 dispatch〔幂等 request_key〕或向该 agent prompt"继续"）→ 全程审计。重启后计划重装（类型一恢复语义）。

边界（诚实约束）：**无 reset_at 不恢复**——显示"限额中，恢复时间未知"，绝不猜时间；恢复动作是"续跑"，不是"验收"；限额暂停是任务级状态，与 S6 的 office 级 pause 分开建模，互不混淆。

验收：
- [ ] 申报 rate_limited + reset_at（可用近未来时间实测）→ 到点自动恢复动作发生且留审计
- [ ] 无 reset_at → 停留且如实显示；reset_at 过期未恢复（server 不在）→ 重启后计划重装并对过期项提示而非盲目执行
- [ ] 与 office pause 互不干扰：pause 下的唤醒计划不触发派发，resume 后按计划继续

## 4. 裁决记录

**V15-4（2026-10-02，Haisu）**：
> V15-4 建议修订为：主 checkout 允许自动 git pull --ff-only（仅干净树），我同意这个。要加一个设置项，可以人工开启自动 pull，或者关闭自动 pull。

落地为：`auto_pull` 设置项（office_settings 持久化，**默认关闭**，CLI 与工作台双入口切换）；开启后方允许主 checkout 自动 `--ff-only` pull；fetch/behind 提示不受开关影响。已并入切片定义与验收。

## 5. 与既有结构的关系

- 全部切片长在 V14 已合并的地基上（S1–S6），不新建架构层——符合宪章 Architecture Order：gh/git CLI、S4 申报通道、S6 调度、V08 服务均为已验证复用点。
- V14 父单 #27 的"PR/checks 查看"由 V15-2 收口；#49（音效/主题/i18n/SSH）保持排后不动。
- V12 资源验收仍在各自切片内按需测量（如 V15-5 的定时器精度不另立性能门）。

## 6. 已验证与未验证

已验证：V14 全量合并且 CI 绿（PR #50）；五缺口为用户一天真实使用的一手结论（2026-10-02 口述）。

未验证：本文件全部为规划，无任何实现；各切片验收为草拟，实现 PR 时以 issue 为准；V15-4 的裁决待确认。

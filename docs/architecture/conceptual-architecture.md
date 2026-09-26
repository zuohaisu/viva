# Viva — Conceptual Architecture

Status: canonical · Date: 2026-09-27 · 上游：`docs/product/product-model.md` · 本文给形状，不给实现

## 1. 两条轴（Viva 与 IDE 的本质区别）

> IDE 主要管理空间。Viva 还必须管理时间。

```text
Spatial axis（在哪工作）              Temporal axis（积累了什么）
Workspace                             Experience (events → episodes)
  └─ Repository                        └─ Reflection
      └─ Worktree                          └─ Memory（workspace 级 / resident 级）
          └─ Session / Worker                  └─ Skill
              └─ Files / Terminal                  └─ Self-Model / User-Model
```

- 空间轴是**包含**关系，回答 VS Code 擅长的 "where am I working" + Orca 擅长的 "where are my parallel agents working"。
- 时间轴是**成长**关系，回答 Hermes / self-model 擅长的 "what happened before / what did we learn / how have we changed / what should persist"。
- 两条轴的交点：**每次空间轴上的工作，都会在时间轴上留下沉淀；时间轴上的沉淀，会改变下次空间轴上工作的方式。** 这个环转起来，Viva 就成立；只转空间轴，Viva 就是又一个 launcher。

关键区分（详见 temporal-model）：**Restore ≠ Recall**。VS Code 式 restore（恢复空间状态）Viva 必须有；Viva 独有的是 recall（带着被策展的意义回来，而不是带着一堆文件回来）。

## 2. 分层与职责

```text
┌────────────────────────────────────────────────────────┐
│ Persistent Self 层（Resident 的状态与成长）              │
│   identity / memory / skills / journal /               │
│   user-model / self-model / relationship               │
├────────────────────────────────────────────────────────┤
│ Resident: Samuel（协调与提案；观察一切 Session）          │
├────────────────────────────────────────────────────────┤
│ Workspace 层（长期项目容器）                             │
│   repositories / project memory(文件优先) /             │
│   tasks / worktree 注册表 / workspace 历史索引           │
├────────────────────────────────────────────────────────┤
│ Worktree 层（执行环境）                                  │
│   branch / task 绑定 / 状态 / diff / 清理纪律            │
├────────────────────────────────────────────────────────┤
│ Worker 层（可替换的工具）                                │
│   catalog / invocation / WorkerSession 登记              │
└────────────────────────────────────────────────────────┘
```

职责规则：
1. **向下只通过稳定小接口**：Resident 对 Workspace 的依赖是"读 project memory + 列 worktrees/tasks"；Workspace 对 Worker 的依赖是"启动 + 登记"。任何一层可被替换（worker 换型号、worktree 换 git/Orca driver、resident 换认知模型）。
2. **成长状态附着在正确的对象上**（product-model Q3/Q5 的裁决）：project memory/skill 附着 Workspace；experience/memory/self-model 附着 Resident。
3. **证据只追加，身份可修正**：journal/append-only；memory/假设可修订（带 supersede）。

## 3. Integrate, don't clone（架构第一原则）

| 已存在的好东西 | Viva 的动作 |
| --- | --- |
| VS Code editing / debug / LSP | 不做。按 worktree 代开 VS Code |
| Git worktree / branch | 直接用 git 原语（复用本仓 `git_worktree.py` 的护栏思路），不发明 branch 系统 |
| Codex / Claude Code / Qoder / Pi 的编码能力 | 不做通用 agent；做 Worker catalog + 受控 invocation + session 登记 |
| Orca 的 worktree-terminal-agent 编排 | 不重造。留作可选执行 driver（集成目标，非依赖） |
| Hermes / agentskills.io 的 SKILL.md | 直接采用格式，互通不互锁 |
| self-model 理论 | 引用字段与更新梯度，不重新发明理论 |
| Plane / GitHub / `gh` | Delivery Automation 的既有连接器保留原位 |

**Viva 真正拥有的东西**（不允许被外包，也不外包给别人）：

```text
long-lived context（Resident 状态 + workspace 状态）
object relationships（workspace↔worktree↔task↔session↔episode）
experience continuity（journal → memory → skill 的准入门与反哺）
resident identity（可迁移、跨模型的 Samuel）
workspace/worktree lifecycle（注册、状态、清理纪律）
worker coordination（catalog、启动、观察、交接）
```

这是 "stitching layer" 的准确含义：缝合是它的位置，**不是它的价值**；价值是上面这六样缝合物本身。

## 4. 形态与所有权

- **单机、本地优先**：所有 Viva 状态在 Haisu 机器上，开放格式（Markdown / JSON / SQLite 均可，但必须是人可读、可备份、可迁移的）。
- **两个存储区**：
  - `~/.viva/`（或等价）——Resident 状态 + workspace 注册表 + journal 索引（跨项目资产）；
  - `<workspace>/` 内——project memory / workspace skills 尽量进 repo 文件（可版本化、worker 可读；VS Code 的 configuration-as-files 原则）。
  - 工作状态（session 注册表、worktree 注册表、episode 索引）属于前者并按 workspace 键控（VS Code 的 state-as-database 原则）。
- **没有云依赖**：不假设网络可用；所有远端交互（GitHub、Plane）是连接器行为，不是运行前提。
- **已定的实现基线**（先行实现轮，`viva-transition.md` 为权威）：TUI 选型 Textual（唯一新运行时依赖，理由与备选分析见 transition §5）；状态布局 `~/.viva/`（transition §4）；权限词汇 `src/viva/permissions/`。进程模型与后续存储选型仍是实现期决策。

## 5. 与 Delivery Automation 的架构关系

Ticket Autopilot 不是平行系统，是 Resident 可调用的一种**结构化工作流**：

```text
Resident（Samuel）
   └─ 发起 / 观察
        └─ Delivery Automation（Ticket Autopilot：intake → dev → checks → QA → bounded fix → owner 授权）
             └─ 在某个 Worktree 上执行，evidence 并入该 Task 的 Episode
```

权威边界不变：worker/automation 永不 push/merge/自授权；owner authority 沿用本仓 delivery_policy 的 actor-aware 记录。能力级映射见 `viva-transition.md`。

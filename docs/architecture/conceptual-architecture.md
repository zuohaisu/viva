# Viva — Conceptual Architecture

Status: canonical · 版本：2026-09-27（AI Office 修订）· 上游：`docs/product/product-model.md` · 本文给形状，不给实现（实现映射见 `docs/architecture/viva-transition.md`）

## 1. 七个必须成立的关系（本轮的最小架构）

```text
① 成员身份与职责
   Resident(identity, history, knowledge) ── role ──► Role（配置数据）
   换 role / engine / tool 都不改变 identity、history、knowledge。

② 成员 ↔ 可替换的认知模型与执行工具
   Resident ── engine binding ──► Engine(model) ── tool ──► Worker(CLI)
   执行时解析为具体的 (engine id, model, tool)，不可用则明确报错，不静默替换。

③ 任务分配（一个任务可给一个或多个成员）
   Task ──assignees[1..n]──► Resident
   分配只写 Task.assignees；成员身份不因分配改变。

④ 每次执行的完整归属
   Execution := (member, task, engine+model, tool, work_location, authorization, request)
   · 启动时固定并持久化；UI 当前选择只影响导航（不变量 I6）
   · request.kind ∈ {user, worker}：界面上看到的来源与记录一致，不可冒充。

⑤ 协调成员的派发 / 观察 / 停止 / 取回
   Coordinator Execution ──grant──► viva office dispatch|status|result|stop
   · 真实 CLI 调用（worker 是子进程，命令是它唯一能触达 Viva 的方式）
   · 子任务只能花父 grant 的子集（不变量 I7）；拒绝写入 authority.refused
   · 不是固定流水线：dispatch 的目标成员与任务由调用方决定

⑥ Workspace ↔ Project ↔ Repository
   Workspace(长期语境) ──► Project(一摊工作) ──► Repository(git 仓库, 多对多显式)
                          └──► Task(意图) ──► Worktree(可写执行环境，按任务隔离)

⑦ 知识的归属与共享边界
   personal_memory / self_model_candidate → 成员私有（只进该成员的任务）
   project_knowledge                      → Project 私有
   team_knowledge / skill                 → 全团队共享
   条目必须带 provenance；只有被后续执行使用过才算"复用证据"
```

## 2. 层与边界

```text
┌──────────────────────────────────────────────────────────────────────────┐
│ Surfaces        TUI（人机主界面）· CLI（人机 + 成员回调 Viva 的唯一接口）    │
├──────────────────────────────────────────────────────────────────────────┤
│ Office control  dispatch · status · result · stop · grant · recover      │
├──────────────────────────────────────────────────────────────────────────┤
│ Domain          members(residents) · tasks · executions · projects ·     │
│                 knowledge · grants · experience · github（只读）          │
├──────────────────────────────────────────────────────────────────────────┤
│ Platform        worktrees（隔离）· workers（工具探测）· store/redaction   │
└──────────────────────────────────────────────────────────────────────────┘
```

边界规则：

1. **向下的依赖只经过稳定小接口**：surface 只依赖 `VivaContext`（composition root）；domain 之间不互相写对方的文件。
2. **成员不是进程**：成员的状态在文件里；进程属于 Execution。
3. **一切写入先脱敏**：journal、执行日志、grant、knowledge 都复用 `viva/core/redaction.py`。
4. **一切远端动作先授权**：`viva/permissions/authority.py` 是唯一入口，成员永不获得 owner 权限。

## 3. Integrate, don't clone（架构第一原则 + 本轮复用检查）

| 已存在的好东西 | Viva 的动作 |
| --- | --- |
| Git worktree / branch | 直接用 git 原语（本轮把旧子系统里被证明的 worktree 服务搬进 `viva/worktrees/service.py`） |
| Secret redaction | 搬进 `viva/core/redaction.py` 并复用（不重写） |
| Owner 授权与交付判定 | 搬进 `viva/permissions/authority.py` 并扩展为 grant（ADR 0007） |
| `gh` CLI（已安装、已认证） | 作为唯一 GitHub 连接器，只读白名单调用；不写自己的 HTTP client |
| Claude Code / Qoder / Codex 等 CLI | 作为 Worker（配置数据）；Viva 不做通用 agent，只做受控 invocation + 执行记录 |
| Orca 的 worktree-terminal-agent 编排 | 不重造；留作可选执行 driver（集成目标，非依赖） |
| self-model 理论 | 引用假设制字段，不重新发明理论；本轮只记录候选 |
| SKILL.md（agentskills.io） | 技能的存储格式（可移植到别的工具） |

**Viva 真正拥有的东西**（不允许被外包）：

```text
成员身份与角色/模型/工具的多对多绑定
Task 作为意图对象的持久性（跨执行、跨成员存活）
Execution 的完整归属与恢复语义（谁在什么时候用什么做了什么）
委派授权边界（grant 的父子关系与拒绝证据）
四类知识的归属与复用证据
Workspace/Project/Repository/Worktree/Task 的对象关系
```

## 4. 形态与所有权

- **单机、本地优先**：所有状态在 Haisu 机器上，开放格式（JSON / JSONL / Markdown），人可读、可备份、可迁移。
- **两个存储区**：`~/.viva/`（成员、任务、执行、授权、知识、experience、worktrees）与仓库内文件（项目知识如需随仓库走，可由 owner 放在 repo 内；Viva 只要求归属正确）。
- **没有云依赖**：GitHub 是可选的只读连接器，不是运行前提；没有网络时一切本地能力照常工作。
- **当前实现基线**：Python 3.11+ / Textual；执行用真实子进程 + 进程组（可并发、可停止、可恢复）。
- **批准的目标**：Rust + Tokio + Ratatui + Crossterm，尚未实施迁移。范围、状态所有权、关闭界面语义和性能验收统一以 [ADR 0011](../decisions/0011-rust-host-and-tui.md) 为准。Pi 已批准为首个默认对话宿主，以交互终端与小型扩展接入，尚未实现；办公室状态存储已批准为 SQLite + 普通文件，尚未实施，与外部记忆分开；具体外部记忆接入与 desktop 框架仍待核对/裁决。

## 5. 与旧产品路径的关系

Ticket Autopilot 已退役（ADR 0008）。它的三个被证明仍有用的能力（worktree 隔离、脱敏、owner 授权）已被搬到 `src/viva/` 的小模块并保留原有验证；其余代码路径、固定流水线与旧产品文档已删除。历史运行数据未被删除，退役清单与复用映射见 `docs/architecture/viva-transition.md`。

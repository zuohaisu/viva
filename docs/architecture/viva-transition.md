# Viva Architecture Transition — 多成员协作改造记录

Status: **authoritative**（2026-09-27，取代本文 2026-09-27 早先的 Ticket Autopilot 过渡版本）。

Rust 宿主的批准目标与当前 Python 实现的区别见 §12 及 [ADR 0011](../decisions/0011-rust-host-and-tui.md)。

本文记录两件事：**（A）** 仓库从 "Ticket Autopilot 作为保留子系统" 迁移到 "个人 AI 协作系统" 的边界变更；**（B）** 旧子系统的能力清点、复用点与退役清单。代码变更以本文为索引，`docs/decisions/0006`–`0010` 是边界决策本身。

## 1. 产品边界的两次变化

```text
最早：Ticket Autopilot = Product
      (Plane ticket → 隔离 worktree → Developer → checks → 独立 QA → 有界修复
       → owner 授权 → 本地 commit；localhost Web 控制器)

上一轮（已作废）：Viva = 单 Resident 的持续性层；Ticket Autopilot 作为保留的
      Delivery Automation 子系统继续存在（ADR 0004）

现在：Viva = Haisu 的本地优先个人 AI 协作系统（ADR 0006）
      - 多个持续存在的 AI 成员（Resident），职责由 Role 配置
      - 模型（Engine）与执行工具（Worker）是可替换绑定
      - Task 是意图；Execution 是一次真实执行并固定自己的归属与授权
      - Ticket Autopilot 已退役（ADR 0008）：只保留三个被证明仍有用的能力
```

核心本体不变量（任何设计都必须保持）：

```text
Resident ≠ Role ≠ Cognitive Engine ≠ Worker ≠ Execution Session
Workspace ≠ Project ≠ Repository ≠ Worktree ≠ Task
raw event ≠ experience ≠ memory ≠ self-model ≠ identity
Session dies. Resident persists.
```

## 2. 旧子系统的能力清点（删除前的地面实况）

`src/ticket_autopilot/`（约 8.3k 行）当时包含：Plane 连接器与票务契约/就绪门槛、prompt resolver 与 planner adapter、固定 Developer→checks→QA 修复循环、run manager 与 run events（run schema）、localhost Web 控制器与静态资源、YAML engine（cli/llm/script/mock drivers + guardrails）、AgentCatalog、development verifier、process output 流、agent session ledger、worktree 服务、delivery policy、脱敏函数。

Viva 当时的真实依赖只有三处（外加一处文字引用）：

| 依赖点 | 结论 |
| --- | --- |
| `services/git_worktree.GitWorktreeService` | 仍需要 → 搬到 `viva/worktrees/service.py` |
| `services/run_events.redact` | 仍需要 → 搬到 `viva/core/redaction.py` |
| `services/delivery_policy`（owner 授权部分） | 仍需要 → 搬到 `viva/permissions/authority.py` |
| `workers.json` 注释里的 "AgentCatalog" 文字引用 | 改写为数据说明 |

## 3. 复用 → 原地搬迁（能力与验证一起保留）

| 能力 | 新位置 | 保留的验证 |
| --- | --- | --- |
| Worktree 隔离（受保护分支护栏、argument-only git、owner 授权 push） | `viva/worktrees/service.py` | `tests/viva/test_worktree_service.py` |
| Secret redaction（键名掩码 + token 形态 + known secrets，含日志文件回写） | `viva/core/redaction.py` | `tests/viva/test_redaction.py` |
| Actor-aware owner 授权 + 诚实交付判定（READY/QA_PENDING/TECHNICAL_BLOCKED…） | `viva/permissions/authority.py` | `tests/viva/test_permissions.py` |

搬迁是**逐字保留语义**的：这些是仓库里被证明过的部分，本轮没有重新发明。

## 4. 退役清单（删除，不迁移）

代码：`src/ticket_autopilot/**`（Plane 连接器、票务契约与门槛、固定流水线、Web 控制器、YAML engine、AgentCatalog、run manager/run events、ticket CLI、reference 目录）、`start-ticket-autopilot`、`ticket-controller` 脚本入口、`tooling/spec_to_issue.py`。

测试：`tests/test_*.py`（18 个文件）、`tests/integration/`、`tests/unit/`、`tests/fixtures/`。

文档：`docs/closed-loop-workflow.md`、`docs/manual-pilot-checklist.md`、`docs/run-event-schema.md`、`specs/`（Ticket Autopilot PRD / v0.1 Specification / agent-ready ticket template / architecture graph）、`overview.md`。

**没有删除的历史**：

- `~/.ticket-autopilot/` 运行状态：Viva 不读、不写、不迁移、不删除；
- 旧运行证据仍留在仓库：`qa-verdict.json`、`tasks/`（AIO-* prompt 与 verdict 归档）、`logs/goal-drift.md`；
- 旧决策以 Superseded + 原因保留（ADR 0001–0005）；
- 被删除的文件在 git 历史与本文的记录中可追溯。

## 5. 当前架构：`src/viva/`

```text
src/viva/
  core/         paths(VIVA_HOME) · 原子私有 JSON 存储 · ids · errors · redaction
  residents/    AI 成员（identity + role + engine + tools）· roles/engines 目录（配置数据）
  permissions/  权限词汇 · actor-aware owner 授权 · grant ledger（委派边界）
  workspaces/   Workspace 注册表与当前选择
  projects/     Project（Workspace 内的项目）+ repositories 绑定
  worktrees/    service（隔离）· discovery（只读）· location（谁需要 worktree 的策略）
  tasks/        Task 注册表 · handoff brief（目标/约束/尝试/产出/未完成）
  executions/   执行注册表（归属、状态、并发、停止、恢复）· runner（真实进程）
  office/       控制面：dispatch / status / result / stop / grant / recover
  knowledge/    个人记忆 · Self-Model 候选 · 项目知识 · 团队知识 · 技能（+ 复用证据）
  github/       只读 GitHub 关联（gh CLI）：issue / PR / checks / reviews 证据
  experience/   append-only experience journal（secret-redacted）
  runtime/      会话指针
  cli/ tui/     两个 surface，共用同一个 composition root（`viva/context.py`）
```

依赖方向：

```text
cli / tui  →  context  →  {residents, permissions, workspaces, projects, worktrees,
                           tasks, executions, office, knowledge, github, experience}
office     →  executions + tasks + residents + worktrees + knowledge + github
core       →  stdlib only
```

**没有任何模块再依赖 `ticket_autopilot`。**

## 6. 每个新组件：先查已有能力，再补真实缺口

| 新组件 | 检查过的已有能力 | 真实缺口 | 结论 |
| --- | --- | --- | --- |
| `executions/` | 旧 `run_manager`/`agent_sessions`（ticket-run 绑定）、`subprocess` | 需要"一次执行固定记录成员/任务/模型/工具/位置/授权，可并发、可停止、可恢复" | 新组件（薄）：Popen + 进程组 + append 记录；不引入调度框架 |
| `office/` | 旧 Web 控制器（HTTP 面）、`gh`/MCP（外部工具） | 协调成员需要**真实可调用**的 dispatch/status/result/stop 接口，且不能伪装成用户请求 | 新组件：CLI + grant 校验；不写自定义 RPC/server |
| `tasks/` | 旧 ticket contract / run state | Task 是意图对象，须跨执行存活并与 Worktree 解耦 | 新组件（薄）：一任务一 JSON 文件 |
| `permissions/grants` | 旧 `delivery_policy`（owner 授权） | 委派需要"来源 + 范围 + 委派关系 + 不可放大"的记录 | 新组件：append-only grant ledger；owner 授权部分**直接复用** |
| `projects/` | Workspace 注册表 | Workspace ≠ Project ≠ Repository（ADR 0009） | 新组件（薄）：项目注册 + 仓库绑定 |
| `knowledge/` | 旧 run events / 旧文档的 experience≠memory 纪律 | 四类知识的归属边界 + "复用证据"的可判定定义 | 新组件：append-only entries + usage；不引入记忆框架 |
| `github/` | `gh` CLI（已安装且已认证）、旧 GitHub 连接器 | 只需只读关联与证据读取，且必须禁止写操作 | 新组件（薄）：白名单式只读 `gh` 调用 |
| TUI 并发 | 旧 `_worker_running` 全局布尔 | 同一个成员要能并行多个执行，停止互不影响 | 用执行注册表取代布尔；归属取自执行记录（bug 修复） |
| `workflows/`（F01） | Task/Execution/grant（V03/V04）、`gh`/git（V08）、task brief、退役流水线的失败经验 | 交付型任务需要可配置的步骤/角色/证据/重试预算，且不能再造固定 ticket pipeline | 新组件（薄）：工作流是数据（步骤×角色×证据×预算×转移）；推进只认绑定真实 head 的证据；保护动作注册期拒绝；无 ticket/run/verdict 对象 |
| `maintenance/`（F02） | knowledge lifecycle/skills（V11）、git 只读检查（V08）、TaskRegistry | 运行期复审只有手动管理；需要带证据、幂等、可逆、授权门控的提议机制 | 新组件：session 窗口 + dedup 提议 ledger；本模块无删除/prune/远端写；worktree 与整洁类仅人审 |
| `tools/computer/`（F03） | `orca computer` CLI（本机已装已授权）、`osascript`、members 探测 | 成员需要经 Viva 定位→动作→核验地操作浏览器/原生应用，带授权与前后证据 | 新组件（薄）：复用现有工具的 argv 适配 + 审计 + task 级授权 + 跨进程前台 lease；不写自动化平台 |
| `memory/`（F04） | 用户实际运行的 Hermes bundled Holographic（`~/.hermes/hermes-agent`，v0.21.4+canary，`fact_store` 工具接口）、knowledge 的 external_ref 机制 | bundled schema 无成员/项目作用域、无来源、无可恢复退出（`remove_fact` 是物理 DELETE） | 新组件（薄）：Python 适配器复用真实 provider；Viva 层补 per-member link + 来源 + usage + archive/restore；不重写 FTS/HRR |

### 6.1 首个独立 Rust 版本的执行体验缺口（2026-09-28，规划）

以下为新规划，不改变 §5–§9 的 Python 现状记录。产品投入使用要求见 [first-usable-version.md](../product/first-usable-version.md)。

| 新模块/组合 | 已检查能力 | 真实缺口 | 交付原则 |
| --- | --- | --- | --- |
| 交互终端 | 当前 runner stdin=DEVNULL；Orca 的公开实现；成熟 PTY/终端库 | 日常 Agent TUI、输入/resize/scrollback 与有界进程生命周期 | V05 核对源码/库及许可证后选择；不因开源就搬 Electron |
| 多 worktree 开发工作台 | 仓库 worktree service/discovery；Git/gh；V02/V03/V05–V09；Orca 行为与源码 | 用户无法只开一个 Viva 即管理多个任务、终端、改动和恢复 | V14 组合现有事实与小接口；不新建工作流状态源，不依赖 Orca.app/外部 CLI 通过验收；Viva 打包拥有的源码 helper 需验证退出与预算 |

候选复用的固定源码与耦合证据见 [Orca reuse audit](../research/orca-reuse-audit-2026-09-28.md)，实现与运行均待验收。

## 7. 持久状态：`~/.viva/`

```text
~/.viva/
  config/       settings.json · workers.json（执行工具）· roles.json · engines.json
  residents/    <member-id>.json           成员记录（identity + role + engine + tools）
  workspaces/   registry.json              Workspace 注册表
  projects/     registry.json              Project 与 repositories
  tasks/        <task-id>.json             任务、产出、未完成事项
  executions/   <execution-id>.json        执行记录（归属/授权/状态）
                logs/<execution-id>.log    原始 worker 输出（0o600，结束时脱敏）
  authority/    grants.jsonl               grant / revocation / refusal 证据
  knowledge/    entries.jsonl · skills/    四类知识 + 技能（SKILL.md）
  experiences/  journal.jsonl              append-only experience
  worktrees/    <repository>/<task-id>/    按任务隔离的可写工作位置
  runtime/      state.json                 当前成员/workspace/会话
```

要求：原子写、0o600/0o700、`schema_version`、secret 先脱敏再落盘、全部可重载、`VIVA_HOME` 可覆盖（测试用）。历史 `~/.ticket-autopilot/` 不被读写。

## 8. 权限与授权（本轮的完整规则）

权限词汇仍是 READ / PROPOSE / ACT_WITH_APPROVAL / ACT_AUTONOMOUSLY / FORBIDDEN（`src/viva/permissions/`），但规则从"一切必须用户发起"扩展为：

1. 用户发起的动作 → ACT_WITH_APPROVAL；
2. 成员发起的派发/停止 → 必须持有效 grant（ADR 0007），否则 FORBIDDEN 并记录拒绝原因；
3. 无 grant 的自主调用 → FORBIDDEN（`invoke_worker_autonomously`）；
4. 远端/受保护动作（push 保护分支、merge、approve PR、自授权）→ 只有 owner 能授权，成员永不获得；
5. 请求来源不被改写：`request.kind` 只能是 `user` 或 `worker`，两者不能互相冒充。

## 9. 诚实约束（反人设表演）

- 成员记录是**配置 + 历史 + 知识**，不是"已证明的完整 Self 连续性"。CLI/TUI 明确这么说，测试锁定这组字段。
- Experience journal 永不被称为 Memory。`personal_memory` / `self_model_candidate` 是**人工策展**的条目，带 provenance；没有用途记录的条目不叫"已复用"，`self_model_candidate` 没有任何自动晋升。
- 没有反思调度、没有自动 Self-Model 演化、没有 Jev 集成：文档与 UI 都写"未实现"。
- 成员名不是代码：`Viva ≠ Samuel` 仍然成立，测试断言核心不出现成员名分支。

## 10. 迁移安全承诺（本轮遵守情况）

- 未删除运行数据（`~/.ticket-autopilot/`、`qa-verdict.json`、`tasks/` 归档）。
- 未删除他人工作：worktree 生命周期仍是人工授权（AGENTS.md），本轮只创建/使用 Viva 自有的 `~/.viva/worktrees/`。
- 被保留能力的验证随代码一起搬走并继续运行。

## 11. 明确不在本轮范围

自动反思循环、Self-Model 演化、Jev 决策辅助集成、daemon、远端写操作（push/PR/merge 自动化）、多用户/云、Orca 作为执行 driver 的集成评估、任何旧子系统的复活。每一项都需要单独、有证据的里程碑。

## 12. Rust 宿主裁决（批准目标，未实施迁移）

2026-09-27，Haisu 批准 Rust 技术方向。唯一权威结论为 [ADR 0011](../decisions/0011-rust-host-and-tui.md)，其中记录实施默认组合 Tokio / Ratatui / Crossterm、已有能力与缺口、所有权、退出与恢复、资源/速度/构建验证。后续批准 Pi 作为首个默认对话宿主，以交互终端与小型扩展接入；身份与长期资产独立于 harness，RPC/SDK 仅为未来可选路径。2026-09-28，用户另批准 SQLite + 普通文件保存 Viva 状态，与外部记忆分开。Rust、PTY、Pi 扩展与 SQLite 状态层均尚未实施；Holographic 的公开 Hermes 实现已通过[专项研究](../research/hermes-holographic-memory-2026-09-28.md)定位；用户实际版本与 Pi/其他 harness 接入尚未验收，研究不构成后端接入批准。

§5–§9 仍是当前 Python 实现及历史改造记录，不是 Rust 已落地的证明。此次文档裁决没有替换 `src/viva/`、数据格式、安装入口或 CI；后续实施可直接替换 Python；旧源码、内部结构与接口没有延续或兼容义务，验收以有效产品需求、用户数据保全及授权边界为准，具体规则只见 ADR 0011 §4。之前的多轮技术研究进入 [历史存档](../research/archive/technology-selection/2026-09-27/README.md)，其中阶段性推荐不再作为现行决策。

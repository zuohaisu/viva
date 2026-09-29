# ADR 0008 — 退役 Ticket Autopilot：只保留当前目标真正需要的成熟代码

Status: **Accepted**（Haisu 显式指令，2026-09-27）· Supersedes: ADR 0004（Ticket Autopilot 作为保留的 Delivery Automation 子系统）

> 效力说明：**"Ticket Autopilot 不再是需要保留的产品能力，只复用当前目标真正需要的成熟代码"是 Haisu 的显式决定**。能力清点、复用点与退役清单是实现结果，记录如下。

## Context

仓库主体曾是 Ticket Autopilot：Plane-first 的 ticket 交付控制器（worktree 隔离 → Developer → deterministic checks → 独立 QA → 有界修复 → owner 授权 → 本地 commit），带本地 Web 控制器、YAML engine、Plane/GitHub 连接器、票务门槛与固定流水线，约 8.3k 行 + 约 150 个专属测试。

新方向（ADR 0006/0007）里没有它的位置：Viva 的通用对象是 **Task / Execution / Grant / Member**，而 Ticket Autopilot 的对象是 **ticket / run / QA verdict / Plane issue**。保留它意味着维护两套并行语义，并让"固定流水线"伪装成"动态调度"。

## Decision

1. **先清点真实依赖，再动手**。Viva 当时对 `ticket_autopilot` 的依赖只有三处：worktree 服务、secret redaction、owner-authorization 语义（外加 `workers.json` 注释里的一处文字引用）。
2. **把仍有用途的能力搬进 `src/viva/` 的小模块，并保留它们的验证**：

| 原位置 | 新位置 | 保留的理由 |
| --- | --- | --- |
| `services/git_worktree.py` | `viva/worktrees/service.py` | 隔离：受保护分支护栏、argument-only git、owner 授权的 push。测试搬到 `tests/viva/test_worktree_service.py` |
| `services/run_events.redact` | `viva/core/redaction.py` | 脱敏：所有 Viva 写入（journal、execution 日志）复用同一实现。测试 `tests/viva/test_redaction.py` |
| `services/delivery_policy`（owner 授权部分） | `viva/permissions/authority.py` | 授权：远端动作仍需 owner 明确授权 + 审计记录。测试 `tests/viva/test_permissions.py` |

3. **退役其余部分**（删除，不迁移）：Plane 连接器与票务门槛、固定 dev→QA 流水线与其 prompt/verdict schema、旧 Web 控制器与 `start-ticket-autopilot` 启动入口、`ticket-controller` 脚本入口、YAML engine、只验证旧行为的测试（`tests/test_*.py`、`tests/integration/`、`tests/unit/`）、旧产品定义文档（`docs/closed-loop-workflow.md`、`docs/manual-pilot-checklist.md`、`docs/run-event-schema.md`、`specs/`、`overview.md`）、Plane 辅助脚本 `tooling/spec_to_issue.py`。
4. **动态调度不得伪装成流水线**：被删除的是"顺序固定、角色固定、状态机固定"的那部分；Viva 保留的是通用 dispatch/stop/status/result（ADR 0007）。
5. **历史不删除**：

- `~/.ticket-autopilot/` 运行状态**不读、不写、不迁移、不删除**；
- 旧运行证据（`qa-verdict.json`、`tasks/` 下的 prompt 与 verdict 归档）留在仓库里作为历史，不被改写；
- 旧决策以 Superseded + 原因保留（ADR 0001–0005）；
- 被删除的代码与文档在 git 历史中可追溯，本 ADR 记录它们的路径。

## Consequences

- 仓库只剩一个产品：`viva`。`pyproject.toml` 只有一个入口点；CI 只跑 `tests/`。
- Viva 不再有"第二套对象语义"：Task/Execution/Grant 是唯一的协作模型。
- 失去的能力（Plane intake、结构化 ticket 契约、QA verdict schema、Web 控制台）如实记为**已退役**，不是"仍支持"。如果未来需要"结构化自治工作流"，它应当作为 Task 的一种 workflow 重新实现，而不是复活旧子系统。
- `logs/goal-drift.md` 记录本次 reprioritization。

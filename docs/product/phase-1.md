# Viva — Phase 1 Scope

Status: canonical · 版本：2026-09-27（AI Office 修订）· 上游：`vision.md` / `workflows.md`

本文分三部分：**已实现并有证据的能力**、**本轮范围之外但已定义的边界**、**明确未实现且不得假装的能力**。

技术方向已批准并**落地**为 Rust 宿主与终端 TUI（[ADR 0011](../decisions/0011-rust-host-and-tui.md)；Python 运行时已于 V13 退役，映射见 `docs/validation/v13-python-retirement.md`）。本文写作时的 Python 实现表是历史记录，保留作对照；现行能力以 `crates/viva/` 及其验收测试为准。Pi 已作为首个默认对话宿主接入（交互终端 + `extensions/pi/` 小型扩展，真实 Pi 端到端待凭证后按 V12 记录）；外部记忆接入仍未实现，不装假 Holographic。SQLite + 文件存储为现行状态层。

## 0. 已实现（历史表：2026-09-27 的 Python 证据在 `tests/viva/`，V13 后能力由 `crates/viva/` 承载并以 `crates/viva/tests/` 为现行证据）

| 能力 | 实现 | 证据 |
| --- | --- | --- |
| 多成员 + 可配置职责 | `residents/registry.py`、`residents/roles.py` | `test_residents.py::test_many_members_with_different_roles_coexist` |
| 模型与工具可替换绑定 | `residents/engines.py`、`resident engine/tools` | `test_changing_the_engine_keeps_record_history_and_knowledge` |
| 不可用即报错、不静默替换 | `ResidentRegistry.resolve_invocation` | `test_unavailable_tool_fails_loudly_and_is_not_substituted` |
| 成员记录不宣称 Self 连续性 | 成员记录字段集合 | `test_member_records_do_not_claim_self_continuity` |
| Task 对象（状态/指派/产出/未完成） | `tasks/registry.py` | `test_tasks.py` |
| 交接简报（目标/约束/尝试/失败/产出/未完成） | `tasks/brief.py` | `test_handoff_brief_carries_goal_constraints_attempts_and_failures` |
| Workspace ≠ Project ≠ Repository ≠ Worktree ≠ Task | `workspaces/`、`projects/`、`worktrees/` | `test_projects.py`、`test_worktrees.py` |
| 执行注册表：归属固定、并发、隔离 | `executions/` | `test_executions.py` |
| 停止单点不影响其他执行 | `executions/runner.py::stop` | `test_stopping_one_execution_leaves_the_other_running` |
| 中断恢复（running/exited/unknown/recoverable，且不重跑） | `ExecutionRegistry.reconcile`、`office recover` | `test_recovery_reports_states_honestly_and_launches_nothing` |
| 委派授权（来源/范围/父子，不得放大） | `permissions/grants.py` | `test_office.py::test_child_grant_cannot_widen_its_parent` |
| 请求来源不被改写 | `permissions/require_invocation_authority` | `test_a_worker_cannot_dispatch_by_pretending_to_be_the_user` |
| 协调成员真实派发真实成员 | `office/control.py` + CLI | `test_real_coordinator_worker_dispatches_a_real_execution_worker` |
| 只读 review | 角色模式 + 执行 mode | `test_scenario_3_alice_reviews_read_only_and_independently` |
| GitHub 只读关联与证据 | `github/client.py` | `test_github.py`、`test_scenario_9…` |
| 知识四类归属 + 复用证据 | `knowledge/registry.py` | `test_knowledge.py` |
| Experience journal（append-only + 脱敏） | `experience/journal.py`、`core/redaction.py` | `test_experience.py`、`test_redaction.py` |
| 旧子系统能力复用（worktree/脱敏/授权） | `worktrees/service.py`、`core/redaction.py`、`permissions/authority.py` | `test_worktree_service.py`、`test_redaction.py`、`test_permissions.py` |

九个验收场景的 Python 端到端证据曾是 `tests/viva/test_acceptance.py`；V13 后逐 issue 验收测试在 `crates/viva/tests/`（真实 UDS 通道、真实 PTY、真实本地 git 仓库、真实 kill -9 崩溃对账）。

## 1. Phase 1 的一句话目标

> **让"成员存在 → 任务分派 → 真实执行（并行/隔离/可停） → 换人或重启后接着做 → 结果与知识留下归属"这个闭环，在 Haisu 的真实项目上成立。**

2026-09-28 新增的 **首个 Rust 可用版本门槛**：必须在 Orca 关闭时，用一次 Viva 启动完成多 worktree、多工具、多终端并行开发与退出恢复。详见 [first-usable-version.md](first-usable-version.md)；这是待实现要求，已有 Python 测试不证明它已达标。

## 2. 本轮刻意的设计选择（不是遗漏）

- **执行用真实子进程，不用调度框架**：一次执行 = 一个进程组 + 一条记录。并发、停止、恢复都从这个事实出发。
- **Worktree 归 Viva 所有**（`~/.viva/worktrees/…`），不落在仓库内部：避免污染 owner 没有选择忽略的仓库。
- **知识只做人工策展**：自动反思、自动衰减、自动晋升都不做。
- **公开接口用 CLI，不写私有 RPC**：worker 是子进程，唯一能触达 Viva 的方式就是命令；这也让"协调成员真实派发"成为可测试的事实。
- **GitHub 只读**：本阶段只需要读 issue/PR/checks/review；写操作（创建 PR、push、merge）仍是 owner 的显式授权动作。

## 3. 下一阶段候选（不在本轮，未承诺）

- 交付类结构化工作流（把"实现→验证→复核"包装成 Task 的一种可复用 workflow）；
- 知识自动衰减与复审提示；
- Orca / MCP / 其他执行 driver 的接入评估；
- Jev 作为可选决策辅助的接入（若 Haisu 需要）。

## 4. 明确未实现（不得在 UI/文案中假装）

```text
自动反思循环            自动 Self-Model 演化        自动 User-Model 演化
自动记忆晋升/衰减       daemon / 常驻调度器          远端写操作自动化（push/PR/merge）
多人类用户 / 团队账号   marketplace / plugin 生态    跨设备同步 / 云
Jev 决策辅助集成        voice / 桌面 / 消息通道
```

产品对外的说法必须停在实际建成的层：journal 不叫 memory；`self_model_candidate` 不叫"学到了"；没有 usage 记录的条目不叫"已复用"。

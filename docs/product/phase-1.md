# Viva — Phase 1 Scope

Status: canonical · 版本：2026-09-27（AI Office 修订）· 上游：`vision.md` / `workflows.md`

本文分三部分：**已实现并有证据的能力**、**本轮范围之外但已定义的边界**、**明确未实现且不得假装的能力**。

技术方向已批准为 Rust 宿主与终端 TUI，见 [ADR 0011](../decisions/0011-rust-host-and-tui.md)。下表仍描述当前 Python + Textual 实现；Rust 迁移、关闭 TUI 后暂停任务/维护的完整协议尚未验收，不能计入已实现能力。Pi 已批准为首个默认对话宿主（交互终端 + 小型扩展），成员可切换 harness；PTY 承载、Pi 扩展与外部记忆接入均尚未实现，具体边界见 ADR 0011 §5。2026-09-28，SQLite + 文件存储方案获批（ADR 0011 §8），数据库状态层与数据转换尚未实现，下表不作为 SQLite 运行证据。

## 0. 已实现（2026-09-27，证据在 `tests/viva/`）

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

九个验收场景的端到端证据在 `tests/viva/test_acceptance.py`（每个场景一个测试函数，使用真实子进程）。

## 1. Phase 1 的一句话目标

> **让"成员存在 → 任务分派 → 真实执行（并行/隔离/可停） → 换人或重启后接着做 → 结果与知识留下归属"这个闭环，在 Haisu 的真实项目上成立。**

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

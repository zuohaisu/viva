# ADR 0007 — 委派授权：协调成员可在用户授予的范围内调用其他成员

Status: **Accepted**（Haisu 显式指令，2026-09-27）· Supersedes: 旧约束"禁止所有 Worker 发起的委派"（viva-transition 旧 §7 与 permissions 的 `require_user_initiated` 全覆盖）· 相关：ADR 0002（actor-aware authority）

> 效力说明：**"禁止所有 Worker 发起的委派"已被 Haisu 明确取代**：协调 Worker 可以在用户授予的任务范围内调用其他 Workers，同时"不将协调 Worker 的请求伪装成用户直接请求"、子任务权限不能扩大。三条都是 Haisu 的显式要求，实现细节（grant 记录字段）是可演进的实现选择。

## Context

旧 Phase 1 把一切 worker 调用都要求 `initiated_by == "user"`，因此结构性排除了"Samuel 分派任务给 Deven"这一类真实工作方式。但直接把权限放开会让一个成员无限放大自己的能力——这正是旧约束想防的事。

需要同时成立的三件事：

1. 协调成员**能**派发（否则 Viva 只是多个互不相干的聊天窗口）；
2. 派发**有边界**：范围来自用户的授予，不可自我放大；
3. 归属**诚实**：这是成员发起的请求，不是 Haisu 发起的请求。

## Decision

1. **Grant 是唯一的委派凭据**（`~/.viva/authority/grants.jsonl`，append-only）：

```json
{"record_type": "grant", "id": "grant-…",
 "source": {"kind": "user" | "worker", "id": "haisu" | "exec-…"},
 "grantee": "samuel", "task_id": "task-…",
 "actions": ["dispatch", "status", "result", "stop"],
 "mode_max": "read_only" | "write",
 "delegated_from": null, "reason": "…"}
```

2. **来源唯一的两种**：`user`（Haisu 亲自授予，无父 grant）或 `worker`（持有父 grant 的成员向下委派，必须写 `delegated_from`）。worker 发起的 grant 没有父 grant 直接拒绝。
3. **子 grant 不能比父 grant 宽**：actions ⊆ 父、mode ≤ 父、task 必须相同。任一违反都被拒绝，并把拒绝原因写入 ledger 与 experience journal（`authority.refused`）。
4. **请求不被改写**：一次派发要么是用户的直接请求（`request = {"kind":"user"}`），要么是成员在 grant 下的请求（`request = {"kind":"worker","id":<execution id>,"grant_id":<grant>}`）。带 grant 的请求**不能**声称用户来源；没有 grant 的 worker 请求是 FORBIDDEN。两条都在 `require_invocation_authority` 里结构性强制，并有测试。
5. **授权随执行下发**：一个成员在某任务上的执行，携带该成员在这个任务上有效的 grant（记录在 execution 的 `authority` 字段，并注入 `VIVA_GRANT_ID` 环境变量），协调成员因此可以真正调用 `viva office dispatch/status/result/stop`。
6. **远端动作仍然只有 owner 能授权**：push/PR/merge/approve 不存在于成员可授予的 action 集合里；`require_user_authorization` 保持原样（actor-aware authority，ADR 0002）。Agent 不得批准或合并自己的 PR。
7. **不是固定流水线**：`office` 的 action 是通用的 dispatch/status/result/stop，任何成员都可以被派到任何角色允许的任务上；代码中不存在 dev→QA 的硬编码顺序（见 `docs/product/workflows.md` §2）。

## Consequences

- `viva.permissions` 从"用户发起"扩展为"用户发起或持 grant"，`invoke_worker_autonomously` 仍为 FORBIDDEN（无 grant 的自动调用不存在）。
- 拒绝是**证据**而不是异常消失：`viva office status` 与 ledger 都能看到 `authority.refused` 的原因。
- 子任务的执行记录里保留 `delegated_from`，任何一次动作都能回溯到授予它的那个人。
- 尚无"grant 自动过期"：本轮靠显式 revoke（`viva` 的 grant 命令 + 记录原因）。过期/配额留待任务量证明需要时再做。

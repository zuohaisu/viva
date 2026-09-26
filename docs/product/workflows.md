# Viva — Workflows

Status: canonical · 版本：2026-09-27（AI Office 修订）· 素材：`customer-zero.md` 场景 S1–S9 · 验收对象：`phase-1.md`

每条 workflow 标注它对对象模型的最低要求（这些就是验收线）和本轮的实现位置。

## 1. North-star loop（本轮的闭环）

```text
open viva
  ↓
members are present（谁在、谁在跑、谁可换）
  ↓
select workspace / project（语境：在哪工作）
  ↓
create or pick a Task（意图：要做什么；可先于 worktree 存在）
  ↓
assign one or more members（可以是一个成员，也可以让协调成员去派）
  ↓
dispatch → Execution（固定的七元归属：成员/任务/模型/工具/位置/来源/授权）
  ↓
work（并行；每个可写任务一个隔离 worktree；成员之间可派发/观察/停止）
  ↓
review（另一个成员只读复核；分歧与结论留在任务上）
  ↓
record（产出与未完成项写回 Task；知识按归属策展）
  ↓
return later / restart
  ↓
recover（如实区分 running / exited / unknown / recoverable；绝不重跑已完成）
  ↓
continue（task brief 带着目标、约束、尝试、失败原因、产出、未完成项交接）
```

**闭环成立的标准**：S1–S9 可真实发生，而不是功能齐全。

## 2. Workflows（与本轮实现对应）

### W1 — 派发与并行执行（← S1）
创建 Task → 分配给成员 → 各自在隔离 worktree 里真实并发。
最低要求：Task 对象 + 成员分配 + 工作位置策略 + 执行注册表。
实现：`viva task new`、`viva office dispatch`；`worktrees/location.py`；`executions/`。

### W2 — 执行归属与界面无关（← S2）
执行在启动时固定归属；切换成员/Workspace 只影响导航。
最低要求：执行记录内含 member/task/workspace，事件从记录派生。
实现：`executions/registry.py`（含 `record_event`）；TUI 完成行取自记录（旧 bug 已修）。

### W3 — 独立只读 review（← S3）
reviewer 以 `read_only` 模式进入同一任务的产出，独立给结论；不写文件。
最低要求：角色模式限制 + 执行级 mode + 同一任务可挂多个执行。
实现：`roles.py` 的 `allowed_modes`；`resolve_invocation`；`test_scenario_3…`。

### W4 — 停止单点（← S4）
停一个执行不影响其他执行（进程组级别隔离）。
最低要求：执行级 pid/pgid + 停止先写记录再发信号。
实现：`executions/runner.py::stop`；`stop_process`。

### W5 — 重启恢复与交接（← S5/S8）
重启后：状态如实分类、可恢复项列出、不重跑已完成；交接简报包含目标、约束、尝试、失败原因、产出、未完成项。
最低要求：执行记录持久化 + 任务级 outputs/unfinished + brief 生成。
实现：`office recover`、`office result`、`task brief`。

### W6 — 模型/工具替换（← S6）
换成员模型只改绑定；历史与知识不动；不可用时报错不替换。
最低要求：engine 绑定 + 历史 append-only + 知识条目独立存储。
实现：`resident engine`、`resident tools`；`resolve_invocation` 的显式失败。

### W7 — 委派与授权边界（← S7）
协调成员在用户授予的任务范围内派发/观察/停止；子授权不能放大；越权留原因。
最低要求：grant ledger（来源/范围/委派关系）+ 请求来源不被改写。
实现：`permissions/grants.py`、`office grant`、`require_invocation_authority`。

### W8 — GitHub 关联与证据（← S9）
Task ↔ Repository ↔ Worktree/branch ↔ PR ↔ issue/checks/review 可追溯；只读。
最低要求：task.github 关联 + 只读连接器 + 证据写回任务。
实现：`github/client.py`、`office.github_evidence`、`github link/evidence`。

### W9 — 知识归属与复用（支撑 W5/W8）
四类知识按 owner 分流；只有被后续执行使用过才算复用证据。
最低要求：kind/owner/provenance/usage。
实现：`knowledge/registry.py`、`knowledge add|use|reuse`。

## 3. 交互纪律

- `viva` 打开 TUI：成员面板 + 语境（workspace/project）+ 任务 + 执行 + journal。**不做** file tree / editor / debugger。
- 命令是交互模式；成员与语境是状态；两者同时存在。
- 需要编辑文件 → 打开 VS Code（Viva 不做编辑器）。
- CLI 与 TUI 共用同一个 composition root，任何能力在两个 surface 上行为一致；**成员回调用的是同一个 CLI**（这是"真实可调用"的含义）。

## 4. 明确不是 workflow 的东西

- **没有固定 dev→QA 流水线**：成员与任务的组合是数据，不是状态机；谁审谁、什么顺序，由 Haisu 或持 grant 的协调成员决定。
- **没有自动反思调度**：知识条目由人策展；`self_model_candidate` 没有晋升流程（未实现）。
- **没有自动远端写**：push/PR/merge 需要 owner 明确授权，且不在本轮自动化范围内。

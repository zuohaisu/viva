# Viva — Project Charter

Status: canonical · 版本：2026-09-27（AI Office 修订）· 取代 `IDEA.md` 早先的 "persistent habitat for a single resident" 表述（原因见 `docs/decisions/0006`）

## North Star

```text
Haisu's AI office should stay real over time:
several AI members, each with its own role, history and knowledge;
tasks that outlive any single worker session;
executions whose attribution and authority are always recoverable;
work that actually gets done through real tools.
```

Viva 是 **Haisu 的本地优先 Personal AI Office**。Viva 不是 AI 成员，也不是单一 Agent 的 wrapper、只有 worktree 功能的工具或记忆数据库——它是成员在其中生活、工作、协作的地方。首个可用版本必须由 Viva 自己提供多 worktree 并行开发入口，不依赖 Orca.app；[投入使用门槛](docs/product/first-usable-version.md)明确了真实验收。

## 核心不变量

```text
Resident ≠ Role ≠ Cognitive Engine ≠ Worker ≠ Execution Session
Workspace ≠ Project ≠ Repository ≠ Worktree ≠ Task
raw event ≠ experience ≠ memory ≠ self-model ≠ identity
Session dies. Resident persists.
```

- **Resident（成员）**：长期存在的 AI 身份（Samuel、Deven、Alice…）。名字与职责是配置数据。
- **Role**：这个成员用来做什么（协调/开发/QA/运维/研究）。配置数据。
- **Cognitive Engine**：此刻为它提供认知的模型。可替换器官。
- **Worker**：具体执行的 agent CLI。可替换工具。
- **Execution**：一次真实运行，固定记录成员、任务、模型、工具、位置、来源与授权。
- **Task**：意图；跨执行、跨成员、跨重启存活。

## Customer Zero

```text
Haisu — 一个人类用户，多个 AI 成员
```

Haisu 是唯一的用户；成员是他创建的记录。产品里没有任何内置人格；`Viva ≠ Samuel`（成员名不是代码）。

## 架构原则

```text
The member must exist before the member thinks.
```

成员的状态（身份、角色、历史、知识）必须独立于当前表达它的模型和当前替它执行的工具存在。换模型、换工具、换角色都不删除成员——这是可检验的（`tests/viva/test_residents.py`、`test_acceptance.py` 场景 7）。

一般化：**复用优先**。先查仓库内外已有的成熟能力，只补真实缺口；每个自建组件都要说明它对照过什么、缺什么（`docs/architecture/viva-transition.md` §6）。

## 产品形状

```text
Viva Core  →  surfaces: CLI · TUI（本轮主界面）· Desktop（未来）
Viva Core  →  capabilities: dispatch/execution · knowledge · recovery · github(只读)
```

宿主与终端界面的批准方向是 **Rust + Tokio + Ratatui + Crossterm**（[ADR 0011](docs/decisions/0011-rust-host-and-tui.md)）；当前实现仍是 Python + Textual，迁移尚未实施。Pi 是首个默认对话宿主，以交互终端与小型扩展接入；成员身份与长期资产独立于 harness。2026-09-28，办公室状态存储批准采用 SQLite + 普通文件，与外部记忆分开。上述迁移与接入尚未实现，技术选择不改变产品与复用边界。

Ticket Autopilot 曾是本仓库的产品主体，2026-09-27 已**退役**（ADR 0008）：其中被证明仍有用的三个能力（worktree 隔离、secret redaction、owner 授权）被搬进 `src/viva/` 的小模块并保留原有验证；其余代码路径、固定流水线与旧产品文档被删除。历史运行数据未被删除。

## 进度定义

进度不是功能数量、成员数量或文档数量。进度是**证据**：一间 AI 办公室能否再多撑住一个边界——多一个成员、多一个并行任务、一次重启、一次换模型、一次越权尝试——并且如实报告结果。

## 诚实约束（不可协商）

没有实现的成长能力不得出现在产品里：journal 不叫 memory，`self_model_candidate` 不叫"学到了"，没有使用记录的条目不叫"已复用"。未知与未实现始终保持可见。

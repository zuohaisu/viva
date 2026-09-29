# Viva — Temporal Model

Status: canonical · 版本：2026-09-27（多成员协作修订）· 上游：`conceptual-architecture.md` §1 · 理论来源：`docs/research/self-model-relationship.md`、`docs/research/hermes.md` · 归属裁决：ADR 0010

## 1. 时间轴要回答的四个问题

```text
what happened before?   → Experience（发生了什么的原始记录）
what did we learn?      → Knowledge（被整理过、带出处的条目）
how have we changed?    → Self-Model candidates（假设，本轮只记录）
what should persist?    → 策展与准入门（什么值得留下）
```

空间轴（Workspace → Project → Repository → Worktree）回答"在哪工作"；时间轴回答"积累了什么"。**两张轴不是同一件事**：空间对象会被清理，时间记录只追加。

## 2. 五个层次必须是不同对象（不变量）

```text
raw event  ≠  experience  ≠  memory  ≠  self-model  ≠  identity
```

| 层 | 是什么 | 谁产生 | 能否被改写 | 本轮状态 |
| --- | --- | --- | --- | --- |
| **raw event** | 一次写入的原子记录（进程输出、命令、状态变更） | 机器 | 否（只追加） | 已实现：`experiences/journal.jsonl` + `executions/logs/*.log` |
| **experience** | 成员经历过的事（事件 + 归属 + 结果） | 机器 + 成员 | 否（只追加） | 已实现：journal 的 event 流（含 execution 归属） |
| **memory** | 被策展过的长期事实 | 人（Haisu 或成员）手工整理 | 是（可撤回，留原因） | 最小实现：`knowledge` 的四类条目，必须带 provenance |
| **self-model** | 关于成员自身的**假设** | 成员提出，人可见 | 是（候选 → 证据 → 撤回） | **只记录候选，不实现演化** |
| **identity** | 成员身份的承诺与配置 | Haisu | 是（显式操作） | 已实现：成员记录的 role/engine/tools |

纪律：**左侧廉价且必须全记，右侧昂贵且必须挣得。** 记录量大不等于成长。

## 3. 沉淀管线（本轮实现的与未实现的）

```text
Execution（真实工作）
   │ 自动留痕（已实现）
   ▼
Event 流（journal + execution 日志，secret-redacted）
   │ 人工策展（已实现，最小）
   ▼
Knowledge 条目（personal / project / team / skill，必须带 provenance）
   │ 人工记录使用（已实现）
   ▼
复用证据（used_in 非空 —— 这是唯一可称为"复用"的门槛）
   │ 自动反思 / 自动晋升（未实现，明确记为后续范围）
   ▼
Skill 晋升 / Self-Model 演化 / User-Model 演化（未实现）
```

**未实现的部分不得被伪装**：没有自动反思调度，没有假设晋升机制，没有任何"学到了/进化了"的 UI 文案。`self_model_candidate` 只是一条带证据的记录。

## 4. 知识的归属与共享边界（本轮的可执行规则）

| kind | owner | 谁能看到 | 归属测试 |
| --- | --- | --- | --- |
| `personal_memory` | 一个成员 | 该成员被指派的任务 | 它描述的是**这个成员的**经验/偏好 |
| `self_model_candidate` | 一个成员 | 同上 | 它是关于**这个成员自己**的假设 |
| `project_knowledge` | 一个 Project | 该项目的任务 | **删掉所有成员它仍然为真** |
| `team_knowledge` | 团队 | 所有任务 | 它描述的是跨成员的协作方式 |
| `skill` | 团队（可绑项目） | 所有任务 | 可复用 + 可验证的程序（SKILL.md） |

共享边界是**默认私有**：个人记忆不会出现在其他成员的任务上下文里；项目知识不会泄漏到别的项目；只有 `team_knowledge` / `skill` 是默认共享层。`for_task(task)` 就是这个边界在代码里的实现。

## 5. 准入门（什么值得留下）

- **Event → Knowledge**（人工策展）：满足其一才值得记录——**复现**（第二次需要同一教训）、**意外**（与既有判断冲突）、**强调**（Haisu 明说记住）、**再推导成本高**。
- **Knowledge → 复用证据**：一次真实使用（`used_in`）。没有使用记录的条目只能说"已记录"，不能说"已复用"，更不能说"已学习"。
- **Knowledge → Skill**：可复用 + 可验证；一次成功不自动成 skill。
- **任何 → Self-Model 候选**：门槛最高（假设 + 证据 + 可见），本轮**只记录不晋升**（数值门槛未定，属后续范围）。

## 6. 对象的时间语义

- **Execution** 是短命的：进程结束（或被停止）后，记录进入终态之一（completed / failed / stopped / exited / unknown）。
- **Task** 是长命的：它跨执行存活，承接 outputs 与 unfinished，是"换个成员接着做"的载体。
- **Resident** 是持续存在的：它拥有历史与知识；换模型/换工具只改绑定。
- **恢复**只做对账（I8）：把"记录说在跑、系统说不在跑"如实分类为 exited/unknown，并给出可恢复项；**从不重启已完成的执行**。

## 7. Restore 与 Recall 的分工

| | Restore（空间轴） | Recall（时间轴） |
| --- | --- | --- |
| 问题 | "上次在哪、开到哪" | "我们之前懂了什么、决定了什么、还欠什么" |
| 机制 | workspace/worktree/execution 状态重放 | task brief + 知识条目 + 未完成事项 |
| 本轮状态 | 已实现（`viva status` / TUI 面板 / `office recover`） | 最小实现（`viva task brief`：目标、约束、尝试与失败原因、产出、未完成、相关知识） |

## 8. 衰减与反哺（仅记录方向，不承诺时间表）

- 衰减：长期未被使用/验证的知识条目降权 → 复审时撤回（`knowledge.retract` 已实现，自动衰减未实现）。
- 反哺：知识进入下一次工作的注入集（`task brief` 的 knowledge 段）；复用被记录为 usage。
- 证据回流：Viva 累积的复用/撤回史未来可回流 `self-model` 仓库的研究文档；本轮不做。

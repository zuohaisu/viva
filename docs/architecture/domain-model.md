# Viva — Domain Model

Status: canonical（对象与关系）· 记录版本：2026-09-27（AI Office 修订）· 裁决出处：`docs/decisions/0006`–`0010` · 本文是概念对象的唯一定义处（不含实现 schema；实现字段见 `docs/architecture/viva-transition.md` §7）

## 1. 对象图（最小架构）

```text
                          ┌───────────────────────────────────────────────┐
                          │ Resident（一个 AI 成员）                       │
                          │  identity · role · history · knowledge        │
                          │  self-model candidates（本轮只记录，不演化）    │
                          └───────┬──────────────┬────────────────────────┘
              binds（可替换）     │              │ assigned to（1..n）
        ┌─────────────────────────▼──┐           │
        │ Role     职责（配置数据）    │           │
        │ Engine   认知模型绑定        │           │
        │ Worker   执行工具（CLI）      │           │
        └─────────────────────────────┘           │
                                                  ▼
 ┌──────────────────────────── Workspace（长期语境）────────────────────────────┐
 │  ┌─ Project（一摊有名字的工作）                                              │
 │  │     └─ Repository（git 仓库；多对多，显式绑定）                            │
 │  │            └─ Worktree（按任务分配的可写 checkout；只读任务不需要）         │
 │  ├─ Task（意图：为什么做；跨执行存活；可被多个执行服务）                        │
 │  │     ├─ assignees ────────────────────────────────────────────────────────┤
 │  │     ├─ work_location（worktree 或只读仓库路径）                            │
 │  │     ├─ github 关联（repo · issue · branch · PR）                          │
 │  │     └─ outputs / unfinished                                             │
 │  └─ Episode（叙事单元）◄── Events（append-only experience journal）           │
 └────────────────────────────────────────────────────────────────────────────┘
                       ▲
                       │ records（每次执行固定自己的归属）
                 Execution（成员 × 任务 × 模型 × 工具 × 位置 × 授权）
                       ▲
                       │ authorized by
                    Grant（来源 = user | worker；范围 = task + actions + mode）
```

## 2. 对象定义

### 2.1 成员侧

| 对象 | 定义 | 关键属性 | 生命周期 |
| --- | --- | --- | --- |
| **Resident（AI 成员）** | 一个持续存在的 AI 成员；Haisu 可以拥有多个 | id、name、role、engine 绑定、tools、created_at、notes | 持久；换模型/换工具不改记录、不清历史、不删知识 |
| **Role** | 该成员用来做什么（PM/协调、开发、QA/review、运维、研究…） | id、title、purpose、allowed_modes、default_mode | 配置数据（`roles.json`），可编辑 |
| **Cognitive Engine** | 当前为该成员提供认知的模型绑定 | id、tool、model、model_flag、args | 配置数据（`engines.json`）；可替换 |
| **Worker（执行工具）** | 具体驱动执行的 agent CLI | name、command、args、capabilities、probe | 配置数据（`workers.json`）；可探测可用性 |
| **Execution** | 一次真实执行 | 见 §2.3 | 短命进程；记录持久 |
| **Knowledge entry** | 四类知识之一（个人记忆 / Self-Model 候选 / 项目知识 / 团队知识与技能） | kind、owner、title、body、provenance、used_in | 只追加；可撤回 |

### 2.2 空间侧

| 对象 | 定义 | 关键属性 | 归属 |
| --- | --- | --- | --- |
| **Workspace** | 长期工作语境，注册单位 | id、name、path、is_git、last_opened_at | 用户注册；长期 |
| **Project** | Workspace 里的一摊工作 | id、name、workspace_id、repositories[] | Workspace 拥有 |
| **Repository** | 一个真实 git 仓库 | 路径（解析后）；可被多个 Project 引用 | Project 引用；显式绑定 |
| **Worktree** | 一个可独立写入的 checkout 执行环境 | path、branch、base、repository、mode | 按 Task 分配；Viva 拥有（`~/.viva/worktrees/`） |
| **Task** | 意图单元（"处理 #150"） | id、title、intent、kind、status、assignees[]、work_location、repositories[]、github、outputs[]、unfinished[] | **Workspace 拥有**；可先于 worktree 存在 |

### 2.3 执行与授权

| 对象 | 定义 | 关键属性 |
| --- | --- | --- |
| **Execution** | 一次执行：谁、在哪个任务、用什么模型与工具、在哪、凭什么、结果如何 | id、task_id、member_id、role、engine{id,model}、tool、work_location{kind,path,branch,mode}、request{kind,id,grant_id}、authority{grant_id,actions,mode_max,delegated_from}、status、pid/pgid、started_at/finished_at、exit_code、output_path、summary、failure_reason、recoverable |
| **Grant** | 让成员（而非 Haisu 本人）能够动作的凭据 | id、source{kind,id}、grantee、task_id、actions[]、mode_max、delegated_from、reason、revoked_at |

Execution 的状态语义（如实区分，禁止含糊）：

```text
running     进程存活（pid + 启动时间双重校验，防止 pid 复用误判）
completed   由启动它的进程观察到 exit_code == 0
failed      由启动它的进程观察到 exit_code != 0
stopped     被显式停止（停止先写记录，再发信号，避免与观察线程竞态）
exited      进程已消失但没有记录到结果（Viva 当时不在运行/被中断）
unknown     连进程是否存活都无法判断（没有 pid 记录）
recoverable 非成功结束且任务仍未关闭 → 可以由新的执行继续（但绝不自动重跑）
```

## 3. 关系裁决（易混点集中回答）

1. **Resident ≠ Role**：成员是持久身份，角色是配置属性；改角色不改变成员的身份、历史与知识。
2. **Role ≠ Engine ≠ Worker**：角色说"做什么"，模型说"用哪个大脑"，工具说"用哪个 CLI"。三者都可替换，替换任一个都不删除成员状态与历史。
3. **Execution ≠ Worker ≠ Session**：Worker 是工具类目，Execution 是一次真实运行（固定归属与授权），Session 是某个界面的运行期。**一个成员可同时拥有多个 Execution**（Deven 同时做 #150 和 #151）。
4. **Task 不被 Worktree 拥有**：Task 可以有 0 个 worktree（研究/规划），也可以有多个（方案对比）。
5. **拒绝是证据**：越权委派被拒绝时写 `authority.refused`（含原因与尝试内容），不是静默失败。
6. **知识归属由 owner 决定，不由层级**：personal → 成员；project → Project；team/skill → 团队（ADR 0010）。
7. **Workspace ≠ Project ≠ Repository ≠ Worktree ≠ Task**（ADR 0009）：多对多关系必须显式声明。
8. **每个执行固定自己的归属**：完成事件与产出按 execution 记录里的 member/task/workspace 记录，**不按界面当前选择**（不变量 I6）。

## 4. 不变量（产品级）

- **I1** 成员状态永远可导出为开放格式；换模型不改变状态语义。
- **I2** Event / Experience / Grant / Knowledge 只追加；修正以新记录 + 撤回/supersede 表达。
- **I3** Knowledge 条目无 provenance 不成立。
- **I4** 成员与自动化无交付权：不 push 保护分支、不 merge、不 approve、不自授权（actor-aware authority）。
- **I5** Worktree 清理必须知情：有未合并产出或未关闭 Task 的 worktree 不被静默清理。
- **I6** 执行的归属在启动时固定：切换成员/Workspace 只影响导航，不影响历史记录。
- **I7** 委派不得放大：子 grant 的 actions/mode/task 只能是父 grant 的子集。
- **I8** 恢复只读：对账可以改状态，永远不重启进程；已完成的执行永远不会被重新启动。

## 5. 显式非对象（防止范围蔓延）

- 没有多人类用户/团队账号/权限体系（多成员 ≠ 多用户）；
- 没有 ticket / Plane / run / QA verdict（随旧产品退役，ADR 0008）；
- 没有 Chat 对象（会话作为执行或界面状态承载）；
- 没有自动记忆/反思/Self-Model 演化对象（本轮只记录候选与证据）；
- 没有 daemon / 常驻调度器（执行由真实进程 + 注册表管理）。

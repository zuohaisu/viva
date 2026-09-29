# Goal Drift Log

This is an append-only record of moments when work in `AI-Operations` diverges from
the project's ticket-driven delivery North Star. Record the divergence without
erasing useful artifacts or rewriting history.

## 2026-07-22 — Ticket automation drifted into Start Prompt comparison

**Status:** Detected and corrected

### Intended outcome

The intended product was a ticket-driven automated development loop, with one Linear
or Plane ticket as the minimum unit. The preferred implementation strategy was to use
existing hooks, MCP capabilities, GitHub Actions, and platform-native automation. A
small custom system was acceptable only if those capabilities could not close the
loop.

### Observed divergence

The work increasingly treated universal Start Prompt design and Start Prompt
comparison as the center of the project. The repository gained a substantial prompt
source tree, evaluation harness, and research track, while the ticket automation
remained a specification rather than an implemented vertical slice.

### Evidence

Repository evidence at detection time:

- `specs/Ticket Autopilot v0.1 Specification.md` contained an approved 1,500+ line
  design for the ticket loop.
- `tooling/start-prompt/` contained 50 tracked files and `research/` contained four
  tracked Start Prompt research documents.
- No tracked `ticket-controller` package or application source directory existed.
- `.workbuddy/memory/2026-07-22.md` described Start Prompt construction as the project
  positioning, turning a side track into an apparent North Star.
- Root `IDEA.md` and `AGENTS.md` only described a broad AI operating-system project,
  which was too vague to reject adjacent work.

### Facts versus inference

**Facts:** The Start Prompt work was a potentially reusable supporting asset, but it
became the active product goal without an explicit priority decision and did not
produce evidence that a real ticket could move further through the delivery loop.

**Inferences:** Likely causes (not directly observed facts):

1. The project-level goal was written too broadly to distinguish core product work
   from interesting AI-agent infrastructure.
2. There was no mandatory check connecting each new deliverable to a stage of the
   ticket loop.
3. The Start Prompt problem offered fast, self-contained artifacts, while end-to-end
   automation required integration decisions and therefore created more friction.
4. The existing Controller specification made “build a system” concrete, but the
   earlier “reuse existing capabilities first” preference was not encoded as an
   implementation gate.

### Impact

- Core implementation did not advance to a real one-ticket vertical slice.
- Research and evaluation work consumed attention without validating the product's
  primary closed-loop hypothesis.
- Project memory began reinforcing the drift for subsequent AI sessions.

### Correction or explicit reprioritization

1. Restore ticket-driven automated development as the project North Star.
2. Park Start Prompt research and comparison as a supporting track; preserve its
   existing artifacts but do not extend it by default.
3. Audit existing Linear/Plane, hooks, MCP, GitHub Actions, and native GitHub
   capabilities before implementing a custom Controller.
4. Build only the smallest missing orchestration needed for one real, low-risk ticket
   to reach an evidence-backed Pull Request and ticket result.

### Guard added

- `IDEA.md` is the concise project charter and priority source.
- `AGENTS.md` requires a visible goal check before research, planning, or
  implementation and again whenever scope changes.
- The Ticket Autopilot specification is explicitly a fallback design pending a
  reuse-first capability audit.
- WorkBuddy's same-day memory now labels Start Prompt work as a research side track.

### Recovery milestone

Produce a reuse-first capability map, choose the smallest uncovered gap, and run one
real low-risk ticket through the thinnest possible end-to-end vertical slice. Success
is runtime evidence from the ticket loop, not another framework or prompt artifact.

---

## Entry template

### YYYY-MM-DD — Short description

**Status:** Detected / Corrected / Accepted reprioritization

### Intended outcome

<The ticket-loop stage and measurable outcome that work was meant to advance.>

### Observed divergence

<What work actually focused on or produced instead.>

### Evidence

<Observable repository paths, commands, timestamps, issue links, or other facts.>

### Facts versus inference

**Facts:** <Observed facts only.>

**Inferences:** <Interpretations, hypotheses, or likely causes; label uncertainty.>

### Impact

<Effect on the ticket loop, safety, schedule, or project priorities.>

### Correction or explicit reprioritization

<The corrective action, or the explicit decision approving a priority change.>

### Guard added

<The concrete rule, gate, or check that prevents recurrence.>

### Recovery milestone

<The evidence-backed ticket-loop outcome that demonstrates recovery.>

---

### 2026-07-28 — AIO-5 垂直切片：实现（Development）

[Goal check] 本阶段推进闭环「Development」，证据 = `.github/workflows/ci.yml` 已写入工作区，定义 push/PR 引擎测试闸门。

**Status:** Corrected

### Intended outcome

用原生 GitHub Actions 为既有 Engine 测试提供可逆、可 PR 的确定性验证闸门；不新建平台、不改动 Engine 或 `ticket-pipeline.yaml`。

### Observed divergence

`.github/workflows/` 原为空，尚无 push 或 pull request 自动执行引擎测试的 CI 工作流。

### Evidence

- `2026-07-28T16:08:28Z`：新增工作区文件 `.github/workflows/ci.yml`。
- 工作流为 `Engine tests`，在 `push` 和 `pull_request` 触发；复用 `actions/checkout@v4` 与 `actions/setup-python@v5`（Python 3.11）。
- 工作流安装 `pyyaml` 与 `pytest`，随后执行 `PYTHONPATH=src python -m pytest src/ticket_autopilot/engine/tests/ -q`。
- 当前仅为本地未跟踪文件；未创建分支、未提交、未 push。

### Facts versus inference

**Facts:** 工作流只新增一个 YAML 文件；`pyproject.toml` 的运行时依赖列表为空，而测试源码导入 `yaml`，故 CI 显式安装 `pyyaml` 与 `pytest`。

**Inferences:** GitHub-hosted runner 上的同一测试命令应成为 PR 的自动验证闸门；仍须由本地验证和 GitHub CI 运行实际证明。

### Impact

推进了真实低风险切片的 Development 阶段，并将确定性验证复用到 GitHub Actions，而非建设新的控制器或连接器。

### Correction or explicit reprioritization

选择 AIO-5 推荐候选「新增 GitHub Actions CI 工作流，在 push/PR 时跑 Engine 测试」：单一交付物、可机检、可通过 `git revert` 回滚。

### Guard added

工作流仅检查既有 Engine 测试；未加入 Plane 连接器、独立 QA 连接器、安全护栏或 Engine/DAG 改动。

### Recovery milestone

完成 YAML 与 Engine 测试的本地确定性验证，取得独立 QA verdict；随后在人工 git 门禁下准备 feature branch/commit/草稿 PR。

---

### 2026-07-28 — AIO-5 垂直切片：确定性验证（Deterministic Verification，第 1 次）

[Goal check] 本阶段推进闭环「Deterministic Verification」，证据 = YAML 解析结果和 Engine 测试命令的实际退出状态。

**Status:** Detected

### Intended outcome

验证 CI 文件为合法 YAML，且既有 Engine 测试命令全绿。

### Observed divergence

YAML 解析成功，但本机 Python 3 环境未安装 `pytest`，故测试命令未能启动；这不是测试失败或 CI YAML 缺陷，仍不能记为通过。

### Evidence

- `2026-07-28T16:08Z`：`python3 -c "import yaml; yaml.safe_load(open('.github/workflows/ci.yml')); print('YAML_VALID')"` 输出 `YAML_VALID`，退出 0。
- `PYTHONPATH=src python3 -m pytest src/ticket_autopilot/engine/tests/ -q` 输出 `/Library/Developer/CommandLineTools/usr/bin/python3: No module named pytest`，退出非零。
- `git diff --check -- .github/workflows/ci.yml logs/goal-drift.md` 退出 0。
- `python` 命令在本机不存在；本地复验须用 `python3`，而 CI 由 `actions/setup-python` 提供 `python`。

### Facts versus inference

**Facts:** CI 已明确安装 `pyyaml pytest`；本机 Python 3 可导入 `yaml`，但没有 `pytest` 模块。

**Inferences:** 在与 CI 安装步骤等价的隔离环境中安装这两个包后，测试应可运行；须以实际复验确认。

### Impact

确定性验证暂未通过，进入有界修复循环第 1/3 轮；未产生源码或 Engine 改动。

### Correction or explicit reprioritization

在 `/tmp` 创建临时 Python 虚拟环境，安装与 CI 同一组测试依赖，再原样运行测试（本地解释器名改为 `python3`）。

### Guard added

不将缺少本机测试依赖误报为通过，也不为此修改 Engine、DAG、依赖平台或连接器。

### Recovery milestone

隔离环境中 YAML 解析与 `src/ticket_autopilot/engine/tests/` 均通过；若仍失败，才修改本票范围内的 CI 文件并复验。

---

### 2026-07-28 — AIO-5 垂直切片：有界修复循环（第 1/3 轮）

[Goal check] 本阶段推进闭环「Bounded Fix Loop」，证据 = 隔离环境复验通过且仓库实现零额外修改。

**Status:** Corrected

### Intended outcome

在不越过 AIO-5 范围的前提下，消除确定性验证的本机依赖阻塞并复验。

### Observed divergence

第 1 次验证仅因本机缺少 `pytest` 无法启动；随后发现 Runbook 简写的 `python -m ticket_autopilot.engine run --mock` 少了当前 CLI 必需的 workflow 位置参数。两者均非 CI YAML、Engine 或测试断言缺陷。

### Evidence

- 在仓库外的临时路径 `/tmp/aio5-ci-venv` 创建虚拟环境，并安装 CI 同款 `pyyaml pytest`；没有写入仓库。
- `python -c "import yaml; yaml.safe_load(open('.github/workflows/ci.yml')); print('YAML_VALID')"`（虚拟环境）输出 `YAML_VALID`，退出 0。
- 精确 CI 测试命令（通过虚拟环境 PATH）：`PYTHONPATH=src python -m pytest src/ticket_autopilot/engine/tests/ -q` 输出 `18 passed`，退出 0。
- 针对 Runbook 所称历史 Engine 测试数：`PYTHONPATH=src python -m pytest src/ticket_autopilot/engine/tests/test_engine.py -q` 输出 `7 passed`，退出 0；目录现包含额外 driver 测试，故完整 CI 命令为 18 passed。
- 修正后的复用 Engine 自检：`PYTHONPATH=src python -m ticket_autopilot.engine run src/ticket_autopilot/workflows/ticket-pipeline.yaml --mock --params '{"ticket_id":"AIO-5"}'` 完成 plan/execute/verify/close，输出 `RETRIES: {2: 2}` 与 `COMPLETED: ['close', 'execute', 'plan', 'verify']`。`--mock` 不调用真实 Plane。
- `git diff --check -- .github/workflows/ci.yml logs/goal-drift.md` 退出 0。

### Facts versus inference

**Facts:** CI 配置本身先安装测试依赖；测试目录当前实测 18 项，`test_engine.py` 单文件实测 7 项；CLI 的 `run` 子命令要求 workflow 参数。

**Inferences:** GitHub Actions 在 push/PR 上安装同一依赖后，应得到相同的测试通过结果；远端运行仍须在 PR 建立后由 GitHub 实际确认。

### Impact

确定性验证已恢复为通过。本阶段只修复本地验证环境/调用方式，没有产生需回滚的产品代码；修复循环消耗 1/3 预算。

### Correction or explicit reprioritization

保持 `.github/workflows/ci.yml` 不变；将本机 `python` 缺失和 mock 命令缺 workflow 参数分别作为环境与 Runbook 文档缺口记录，不扩展为 AIO-6/7/8/9 实现。

### Guard added

后续本地 CI 等价复验在隔离环境执行；Engine mock 运行始终显式传入 workflow 路径和 `--mock`，避免意外触发真实 driver/Plane handler。

### Recovery milestone

取得独立 QA 的 `accept` verdict；失败则在剩余 2 轮预算内处理 AIO-5 范围内的问题，缺少独立 QA 能力则如实标记 `BLOCKED_NEEDS_HUMAN`。

---

### 2026-07-28 — AIO-5 垂直切片：独立 QA（Independent QA）

[Goal check] 本阶段推进闭环「Independent QA」，证据 = AC 静态预检结果、实际 QA driver 配置检查及不可满足时的显式 reject verdict。

**Status:** Detected

### Intended outcome

由独立的 Engine `verify` 节点（llm/hermes）或另一 agent/人对 CI 变更给出 `accept`/`reject` verdict；QA 只判断，不修复。

### Observed divergence

AC 的静态预检全部通过，但当前会话没有可用的 LLM 或 Hermes QA driver，且没有另一位独立 QA 审核者。不能把同一执行者的复查或 `--mock` 中预置的 accept 当成独立 QA。

### Evidence

- AC 静态预检输出：`{"acceptance_precheck":{"workflow_exists":true,"push_trigger":true,"pull_request_trigger":true,"dependency_install":true,"engine_test_command":true,"python_311":true},"all_pass":true}`。
- 环境可用性检查（不暴露凭据）：`XY_LLM_BASE_URL`、`XY_LLM_API_KEY`、`XY_LLM_MODEL`、`HERMES_API_URL`、`HERMES_API_KEY`、`HERMES_MODEL` 均为 `UNSET`。
- `src/ticket_autopilot/workflows/ticket-pipeline-hermes.yaml` 的 `verify` 节点使用 `verifier`，其 driver 为 `hermes` 且要求 JSON verdict；真实运行会在缺少 gateway/凭据时失败。`--mock` 的预置 reject/accept 仅测试控制流，非独立审查。
- 本阶段 verdict（仅记录，不覆盖现有 AIO-4 的 `qa-verdict.json`）：
  ```json
  {"schema_version":"1.0","issue_key":"AIO-5","qa_attempt":1,"decision":"reject","reason":"BLOCKED_NEEDS_HUMAN: no configured independent llm/hermes QA driver and no separate reviewer","acceptance_precheck":"pass","deterministic_verification":"pass"}
  ```

### Facts versus inference

**Facts:** CI 结构检查和确定性测试均通过；当前环境未配置可调用的独立 QA driver；仓库已有的 Engine mock verdict 是确定性 fixture，不是独立判断。

**Inferences:** 配置 AIO-7 所规划的独立 QA connector，或由人类/另一 agent 审核此 diff，能够解除该门禁；在未实际运行前不应宣称其会 accept。

### Impact

AIO-5 未能获得 Independent QA 的 accept，因此不得进入 PR 创建或 Plane 状态更新。此为能力/授权缺口，而非可通过修改 CI YAML 修复的问题。

### Correction or explicit reprioritization

`BLOCKED_NEEDS_HUMAN`。不消耗剩余修复预算去重建 AIO-7：那会违反本票「只复用」与 out-of-scope 约束。请求人类提供独立审阅，或显式授权并配置已存在的 LLM/Hermes QA 服务。

### Guard added

拒绝把同一 agent 的静态复核、mock fixture、或测试绿灯冒充为独立 QA verdict；不修改 AIO-7 QA connector、不新增 QA 平台。

### Recovery milestone

独立审核者对同一 diff 给出 `decision: accept` 并附 AC 证据后，才可在人工 git gate 下创建 feature branch/commit/草稿 PR；随后由授权的 Plane MCP 更新 AIO-5/候选工单状态。

---

### 2026-07-28 — AIO-5 垂直切片：PR（Pull Request）

[Goal check] 本阶段推进闭环「Pull Request」：证据 = 已验证的本地 diff 已就绪、但因 QA reject 与人工 git gate 未满足而未创建提交、push 或 PR。

**Status:** Detected

### Intended outcome

在 feature branch 上准备本地 commit，等待人类明确批准后才 push 并创建 draft PR；绝不 push `main`、绝不自动 merge。

### Observed divergence

PR 前置的 Independent QA verdict 为 `reject/BLOCKED_NEEDS_HUMAN`，且尚未取得针对 git 写操作的人工批准。因此没有进入 branch/commit/push/`gh pr create` 操作。

### Evidence

- 工作区交付物：未跟踪 `.github/workflows/ci.yml` 与已修改 `logs/goal-drift.md`；确定性验证证据见本日志前两段。
- 本次会话未执行 `git switch -c`、`git commit`、`git push`、`gh pr create` 或任何 merge 命令。
- 起始分支为 `main...origin/main [ahead 9]`；因此即使 QA 通过，也必须先经人工批准使用隔离 feature branch，不能直接 push main。

### Facts versus inference

**Facts:** 没有 PR URL、commit SHA 或远端 CI run；QA verdict 未 accept。

**Inferences:** 在独立 QA accept 和人工批准后，可用原生 `gh` CLI 创建 draft PR，并由 GitHub Actions 提供远端 CI 证据。

### Impact

PR 阶段安全地停在人工门禁前；AIO-5 的 Definition of Done 中「PR 已建或人工门禁下 diff 就绪 + 决策已记录」的替代路径已具备 diff/决策证据，但完整闭环仍未完成。

### Correction or explicit reprioritization

保持本地 diff，不创建提交或 PR。等待独立 QA accept 后，请人类明确授权创建 `slice/aio5-ci` 分支和本地 commit；再单独授权 push/draft PR。

### Guard added

PR 必须同时满足独立 QA accept、显式人类 git 授权和 feature branch；禁止自动 push main/merge。

### Recovery milestone

得到独立 QA accept 和人类 git 授权后，记录 feature branch、commit SHA、draft PR URL 与 GitHub Actions run 结果。

---

### 2026-07-28 — AIO-5 垂直切片：工单状态更新（Status Update）

[Goal check] 本阶段推进闭环「Ticket Status Update」：证据 = 前置 PR/QA 门禁和本会话 Plane 写入能力状态已核对，未执行不安全的状态变更。

**Status:** Detected

### Intended outcome

PR 就绪并有独立 QA accept 后，使用 Plane MCP 将候选工单及 AIO-5 推进到 In Review，并在工单中关联 PR 和证据。

### Observed divergence

不存在 PR，也没有 QA accept；此外，此会话工具面未暴露 Plane MCP 写入函数。不能以用户提供的旧状态信息、Engine mock 的 `closed: true` 或本地日志代替真实 Plane 写入成功。

### Evidence

- 本日志 PR 段无 PR URL/commit SHA；Independent QA 段 verdict 为 `reject/BLOCKED_NEEDS_HUMAN`。
- 本会话可用工具仅提供本地文件/命令操作，未提供 Plane MCP 调用；未尝试 API 绕过或手工伪造状态更新。
- Engine mock 输出的 `closed: true` 已在本日志有明确标注为 mock，不是 Plane 状态证据。

### Facts versus inference

**Facts:** 未对 Plane 执行读/写操作，故没有新的 Plane 状态或评论证据。

**Inferences:** 获得 PR 和独立 QA 后，具备 Plane MCP 写入权限的授权会话可按 Runbook 更新工单；AIO-8 的参数化 Plane→PR 连接器仍是应单独处理的自动化缺口。

### Impact

工单状态维持不变；这避免了在未满足闭环证据门槛时错误标记 In Review/Done。

### Correction or explicit reprioritization

`BLOCKED_NEEDS_HUMAN`，等待独立 QA、PR 人工 gate 和可用的授权 Plane MCP 会话。只记录缺口给 AIO-7（独立 QA）与 AIO-8（Plane→PR/status glue），不在 AIO-5 实现它们；AIO-6 drivers 专属测试与 AIO-9 强制护栏亦保持 out of scope。

### Guard added

Plane 状态更新要求真实 PR URL、独立 QA accept 和授权 MCP 写入回执；mock 输出和本地自报一律不构成状态更新证据。

### Recovery milestone

在授权会话中把 PR 链接和确定性/QA 证据写入 Plane，并记录实际状态、时间戳及链接；AIO-5 仅在证据齐全后由人类置 Done。

## 2026-07-29 — AIO-7 实现路径与工单原文的三处偏离（用户拍板，非漂移放任）

**Status:** Detected, user-approved, merged back into AIO-7 scope

### What happened

会话从「qodercli 做 QA agent」的口头需求出发，先后实现了双 agent 互备与角色重排，
偏离了 Plane AIO-7 工单原文（tasks/AIO-007-dev-prompt.md）。经对照检查后由用户逐项拍板，
将偏离收敛回 AIO-7 的承重契约（qa-verdict schema + 无伪成功门禁 + connector 接入）。

### Deviations（均经用户确认）

1. **QA agent 是 qodercli 而非 codex**：工单原文要求 codex 做 Verify 独立 QA；用户决定
   codex 改任 planner，QA 由 qodercli 承担且不设备份。独立性不变（QA 独立于开发 agent
   claude/pi）。connector 因此命名为 `connectors/qa.py`（非 codex_qa.py）。
2. **engine.py 新增 `_run_node` fallback**：工单要求「不改循环语义」。fallback 是 driver
   调用外层的附加 try/except（主 agent 失败→备份 agent 一次→双失败照常抛错），未触碰
   `when`/loop/readiness/max_retries。用户知情并要求保留（executor claude→pi 互备依赖它）。
3. **schema 校验为零依赖手写子集而非 jsonschema 库**：环境未安装 jsonschema，且项目
   dependencies=[] 是刻意约束（llm driver 用 urllib 同理）。校验覆盖 type/required/
   properties/items/enum，足够 §9.1 契约；单测覆盖畸形输出一律 reject(BLOCKED)。

### Bug found and fixed during merge

Engine 存在真实门禁缺陷：所有节点初始 stale=True，导致 `close` 在 verify 首次 reject 后
即运行（mock log 实证：close 出现在第 5 步，早于最终 accept）。修复：入边全部带 `when`
的节点初始不置 stale，必须等条件边真正触发。此前「COMPLETED 含 close」掩盖了该缺陷；
新增回归测试 test_schema_invalid_pass_never_reaches_close。

### Evidence

- 40 unittest 全绿（schema 校验 8、connector 无伪成功 7、端到端门禁 2、既有 23 不回归）。
- 两个 workflow mock 运行的节点序列均为 plan→(execute→verify)x3→close，close 仅在
  accept 后出现。
- 无伪成功不变量：仅「schema 合法且 verdict==PASS」产生 accept；CLI 失败/非 JSON/校验
  不过 → reject + BLOCKED verdict（本身 schema 合法，可归档）。

### Guard added

到达 close 的充要条件由引擎结构性保证（条件门控节点不再初始就绪），不再依赖 QA 输出
的自觉；qa-verdict 未过 schema 校验绝不映射为 accept。

---

## 2026-09-27 — North Star 由 ticket-driven delivery 显式变更为 Viva（Accepted reprioritization）

**Status:** Accepted reprioritization（repository owner 显式决策，非漂移）

### Intended outcome

原 North Star：ticket-driven automated software delivery（一张 ticket 为最小闭环单元，IDEA.md 2026-07 版）。

### Observed divergence

无越轨行为——这是 owner 主动的产品边界变更：Viva 成为产品框架（"Haisu 与常驻 Agent Samuel 跨 Workspace/Worktree/Worker/模型协作、连续积累的本地开发环境"），Ticket Autopilot 降级为其中的 Delivery Automation 子系统。本条目按模板记录这次显式 reprioritization，防止未来的 agent 会话把两个 North Star 混淆。

### Evidence

- 2026-09-27 产品定义轮产出：`docs/product/`（vision / customer-zero / product-model / workflows / phase-1）、`docs/architecture/`（conceptual / domain-model / temporal-model / viva-transition）、`docs/decisions/`（ADR 0001–0005）、`docs/research/`（vscode-workspace / orca / hermes / self-model）。
- 先行实现轮（本轮之前，未提交）：`src/viva/`（~2.7k 行）+ `tests/viva/`、pyproject 更名 `viva`（Textual 为唯一运行时依赖）、README/AGENTS/IDEA 已改写为 Viva 框架——树中未提交改动在本次记录时点可见。
- Ticket Autopilot 资产原样保留：`src/ticket_autopilot/` 未改名，`docs/closed-loop-workflow.md` 仍为其操作定义。

### Facts versus inference

**Facts**：以上文件与改动均在树中存在；`src/ticket_autopilot/` 无包名变更；goal check 机制在 AGENTS.md 中已改为 Viva 能力导向并保留子系统内 ticket-loop 变体。
**Inferences**：本次变更消除了"IDEA.md 旧 North Star 与实际工作方向"之间的持续张力；若不记录，后续会话存在把 Ticket Autopilot 当作仍居首位的误判风险。

### Impact

- 交付闭环不再是产品终点，而是 Viva Delivery Automation 子系统的能力；其 bounded-automation 与 evidence-first 纪律晋升为全局不变量。
- Start Prompt 轨维持 parked 不变。

### Correction or explicit reprioritization

Reprioritization 本身：North Star 变更为 Viva（ADR 0001、0004）；Ticket Autopilot 边界冻结在新定位内（ADR 0004）。

### Guard added

- ADR 目录（docs/decisions/）承载边界级决策；AGENTS.md 指向它。
- 两个 goal check 变体各管其区：Viva 能力导向（全仓默认）与 ticket-loop（Delivery Automation 子系统内）。
- "为 hypothetical users 增加复杂度默认不做"写入 ADR 0001。

### Recovery milestone

Phase 1 验收（docs/product/phase-1.md §5）：north-star loop（进入 workspace → 续上下文 → worktree → worker → 工作 → 换人 → 留记录 → 回归继续）在 ≥1 个真实 workspace 上跑通 ≥3 个工作日，S1/S4/S8 场景可真实发生。

---

## 2026-09-27 — Reprioritization: 从"单 Resident 持续性层"到"多成员 AI 协作"（accepted）

### Trigger

Haisu 的任务书明确取代了上一轮产品定义的六条约束：只围绕 Samuel 一个 Resident 设计；把 multi-resident UX 与 AI 团队协作排除在范围外；用"单人类用户"推出"没有 AI Team/Member"；强制用 Workspace 取代所有 Project 概念；禁止所有 Worker 发起的委派；整体保留 Ticket Autopilot 作为 Viva 子系统。

### Scope

产品模型、对象关系、权限模型、执行模型、旧子系统处置、文档与 ADR 体系。不是新增功能轮，而是**纠正方向 + 交付最小可验证闭环**。

### Evidence

- 新决策：`docs/decisions/0006`（多成员 AI 协作）、`0007`（委派授权）、`0008`（Ticket Autopilot 退役）、`0009`（Workspace≠Project≠Repository≠Worktree≠Task）、`0010`（知识归属与复用证据）。
- 被取代的旧决策保留并标注：ADR 0001（部分）、0002、0003、0004 头部均有 Superseded + 原因；0005 保持 Proposed 且不受影响。
- 实现：`src/viva/` 新增 `residents/{roles,engines}`、`projects/`、`tasks/`、`executions/`、`office/`、`knowledge/`、`github/`、`permissions/grants.py`；TUI 的全局 `_worker_running` 与完成事件归属 bug 一并修复。
- 复用与退役：`worktrees/service.py`、`core/redaction.py`、`permissions/authority.py` 三个能力从 `src/ticket_autopilot/` 原地搬迁并保留验证；其余代码路径、固定流水线、Web 控制器、Plane 连接器与旧产品文档退役（`viva-transition.md` §4）。
- 验收：`tests/viva/test_acceptance.py` 覆盖九个场景（真实子进程）；`tests/viva/test_office.py` 含一次真实协调 Worker 调用真实执行 Worker 的验证。

### Facts versus inference

**Facts**：上述文件、测试与删除清单在树中可见；`src/ticket_autopilot/` 已不存在；`pyproject.toml` 只剩 `viva` 一个入口点。
**Inferences**：不一致的旧文档若继续作为权威，会把后续会话引回"单 Resident + Ticket Autopilot 保留"的旧边界；本次纠正消除了这一张力。

### Impact

- 交付自动化的具体实现不再是产品的一部分；它作为**能力来源**的三项被吸收，其余按 ADR 0008 退役。
- 协调 Worker 从"禁止委派"变为"在用户授予范围内委派并留痕"。
- Workspace 不再是 Project 的替身；成员与语境成为两个正交维度。

### Correction or explicit reprioritization

Explicit reprioritization（Haisu 决定）。ADR 0006–0010 是记录；被取代的决策保留历史出处。

### Guard added

- AGENTS.md 新增 **Retired Subsystem Rule**：不得重新引入 ticket/Plane/run/QA-verdict 对象，不得把固定流水线改名为动态调度。
- 委派边界有代码级强制（`require_invocation_authority`、grant 的父子不放大）并有测试；越权必须留下 `authority.refused`。
- 诚实约束加固：成员记录字段集合被测试锁定；`self_model_candidate` 无自动晋升；"复用证据"以 usage 记录为唯一门槛。

### Recovery milestone

九场景验收（`docs/product/phase-1.md` §0）全部有测试证据；下一步是让这套闭环在 Haisu 的真实仓库（非测试夹具）上连续使用多个工作日，并据此校正知识策展与工作位置策略。

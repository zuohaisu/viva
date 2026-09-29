# F01–F04 最终验收记录（issues #23 #24 #25 #26）

验收日期：2026-09-29。本记录针对独立 QA 复审（REJECT @ f550171）的整改后状态，
全部证据为**本机真实运行**的落盘记录（`evidence/f01-f04/`），非模拟输出。
证据生成环境：macOS (darwin 25.5.0 arm64)，worktree `worktrees` 对应分支
`zuohaisu/dev5`。真实验证证据 JSON 的生成时间戳在文件内。

整改范围内的前置事实：

- CI rust / pi-extension 两项硬门禁在 f550171 为红（fmt 差异 + 测试文件未随
  f03/f04 提交），已在 `45bda21` 修复并全绿（runs 36470438127 / 36470438325）。
- 下文逐条对应 QA 的 14 条验收判定与最小修复清单，标注每条的处置与证据。

## 处置总览

| QA 发现 | 处置 |
| --- | --- |
| CI 两项红 + 工作区脏 | 已修复（45bda21），CI 三项 pass |
| #23 C1 无可达入口 | **已修**：`viva workflow register/start-run/record/status/history/resume/abort` CLI；本 PR 交付过程本身经真实走查记录（见下） |
| #23 C2 head 自报 | **已修**：CLI `start-run --cwd` 经 `git rev-parse`（`git::cli::head_sha`，QA 指出的未使用函数）锚定真实 head；证据全部绑定该 SHA |
| #23 C3 第二交付状态源 | **部分采纳，拒绝回写**：run 状态是过程事实（V03 launch_intents 分层语义先例），回写会让 paused/completed 改写任务状态、违反"结果永不关闭任务"。落地：schema/模块文档明示 + `workflow status` 并列输出 task_status + 既有测试断言 run 完成后 task 仍 open |
| #23 C4 deliver 门可被空 requires_evidence 绕过 | **已修（结构性）**：任何 dispatch 类 action 的 pass 一律强制 authorization 证据 + authority 检查（`AuthorityEngine::is_dispatch_action`），与配置声明的证据清单无关；f01 测试更新为全部 dispatch 步骤持 grant |
| #24 dedup 永久静默 | **已修**：dedup key 增加 generation（该 subject 的 executed 计数）——执行后条件重入（技能重启用仍闲置）产生新提议；dismissed 是常设人决、保持沉默并有测试覆盖 |
| #24 disable_skill 无效 | **已修**：knowledge 选择路径 join `skills.enabled`，停用的技能真实离开任务上下文（新测试 `disabled_skills_leave_task_context_selection`） |
| #24 dismiss 无授权 | **已修**：dismiss 与 execute 同样要求 live `maintain_knowledge` grant，拒绝入审计日志（f02 测试更新） |
| #24 无入口/无运行主体 | **已修**：`viva maintenance review-knowledge/review-worktrees/review-repo/proposals/execute/dismiss` CLI；每次评审即一个有界 session（开→扫→关），无 daemon |
| #25 重言式核验 + 丢截图 | **已修**：verify 双轴——动作自身输出必须含观察标记（`expect_action_output`）且 post 状态含世界标记；smoke 去掉 `--no-screenshot`，截图作为前后证据的一部分 |
| #25 grant 放大（无 task 限定 + 无 principal 比对） | **已修（principal 绑定）**：`evaluate` 比对 `principal_member_id`，非本人 grant 即 `NotPrincipal` 拒绝并记录；**保留** office 级 grant（task=None）形态——它是 F02 维护所依赖的 owner 直签形态，principal 绑定后滥用面收敛到持有者本人，不可再冒充他人 |
| #25 lease 进程内 + foreground 无可达路径 | **已修（lease 跨进程）**：租约改为 VIVA_HOME 库内 `foreground_leases` 行（写事务即跨进程门），两个独立连接/进程共享同一文件即互斥；f03 测试以两个连接验证。foreground 输入路径仍只在库层提供（真实键鼠注入需要真实任务 + owner grant 时启用），证据文档如实标注 |
| #26 开箱即死 + status 假就绪 | **已修**：python 未显式配置时优先探测 checkout 自带 venv（实测系统 python 缺 ruamel）；`memory status` 现在真实调 adapter（list round trip）并报告 `hrr: active/degraded`（NumPy 降级可见），不再是文件存在性断言 |
| #26 写侧隔离被偷 | **已修**：link 改为 `(fact_id, member_id)` 唯一（migration v2），Bob 写同内容获得**自己的** link 与来源；exit 双门——owner 条件（只能退自己的 claim）+ live `maintain_knowledge` grant；本机真实复现并落证据 |
| #26 link_fact 零调用者 | **已修**：`viva memory link --fact <id>` 资产引用采纳入口 + 真实证据（carol 采纳后可召回） |
| #26 改写召回/provider 高级接口 | **如实记录边界（不可修）**：FTS 候选门控决定同义改写与 CJK 部分串不召回——这是 provider 机制事实，不伪造；产品选择已写入 README（可考虑 probe/related 预扩展或接受边界）。已暴露 provider 真实 `probe` 实体召回（`viva memory probe`）与 HRR 降级可见性；`contradict/fact_feedback` 留待产品裁决，不冒充已用 |
| B1 无验收证据文档 | **已补**：本文档 + `evidence/f01-f04/*.json`；`viva-transition.md` §6 补四行（组件/已查能力/缺口/结论） |
| B2 f04 默认绿 | **已修**：5 个真实 provider 测试改 `#[ignore = "..."]`——CI 上显示为 ignored（可见、不计 PASS），本机以 `cargo test --test f04_memory -- --ignored` 真跑（5/5 通过），输出即证据 |
| B4 拆 4 PR | **不修，理由**：本任务由仓库 owner 明确指示"每个 issue 单独 commit、4 个 issue 统一提交 PR"，owner 指示优先于 issue 模板；以各 issue 证据回链评论补偿 |

## 真实证据清单（evidence/f01-f04/）

全部由 CLI/测试在本机真实运行产生；F01/F02/F04 使用临时 VIVA_HOME 与临时记忆库，
F03 冒烟为只读窗口检查，用户真实 `~/.hermes/memory_store.db` 在 F04 全程未被触碰
（适配器 fail-closed：无显式 `--db` 拒绝运行）。

- `f01-implement/verify-fail/fix/verify-pass/review/deliver/history.json`：
  一张真实低风险任务（本 PR 的整改工作）走完
  实现→验证（真实失败一次）→修复→复验→独立复核→授权交付 的完整过程记录；
  所有证据绑定 `git rev-parse HEAD` 的真实 SHA；merge 保持**待 owner** 状态，
  与验收条件"未经 owner 精确授权绝不 merge"一致。
- `f02-review-knowledge.json` / `f02-execute.json`：注入真实过期知识条目后
  CLI 评审产生 1 条带证据提议，owner grant 执行后条目真实归档（SQLite 状态
  `archived`）；`f02-review-repo(-2).json`：脏工作区提议一次、复跑零重复。
- `f03-smoke.json`：真实 `orca computer` 链路，browser-chrome 与 native-finder
  均 `executed`，动作自身输出含观察标记（windows/snapshot），含截图证据。
- `f04-status.json`：真实探测（venv 自动发现、HRR active、list round trip）。
- `f04-remember/search(-bob/bob-2/bob-after-archive/carol)/archive.json`：
  写入→本人召回→**他人写同内容获得独立 link**→owner-scoped 归档→
  跨 harness 资产引用采纳（carol 召回）。

## 测试与静态检查（本机，整改后 head）

- `cargo test --workspace`：19 个测试目标全部通过，0 失败（含新增的
  principal 绑定、写侧隔离、owner-scoped exit、structural deliver 门、
  dedup generation、skill 停用生效、跨进程 lease 测试）。
- `cargo test --test f04_memory -- --ignored`：5/5 真实 provider 测试通过。
- `cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets
  -- -D warnings`：通过。
- `extensions/pi`：`npm test` 20/20、`tsc --noEmit` 通过。

## 未跑/边界（诚实声明）

- 真实 owner 授权 merge 后的默认分支终验未发生（merge 权在 owner，PR 保持
  待 owner 状态——这正是验收条件要求的诚实路径）。
- 键鼠/焦点类前台注入在真实桌面上的执行未演示（引擎与跨进程租约已测试；
  真实注入留待具体任务 + owner grant）。
- Hermes 多 profile、拥挤 store 召回挤出、p95 延迟基准未做（QA 同样未列为本
  轮门槛）；`contradict`/`fact_feedback` 未接入（产品选择待裁决）。

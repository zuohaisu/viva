# F01–F04 验收记录（issues #23 #24 #25 #26）

> 以下第一、二轮记录为当时的证据快照，**不是当前合并许可**。第三轮独立 QA
> 判定为不建议合并；本轮补丁的单测不替代独立复验、真实前台输入或 owner 合并决定。

## 第五轮独立 QA 后续整改（R5-N1，待独立复验）

旧两列表的非空租约经 v3 修复后无 PID，仍不得自动回收。现在可用 `viva tools computer lane` **只读列出全部**持有者、PID、标记及取得时间；它不触发租约 reconcile，但打开 Viva 数据库时仍会应用未执行的版本化迁移（包括 v3）。确认原动作确已停止后，由持有该 task 的 owner-issued、指名成员、ACT_WITH_APPROVAL 的 `release_foreground_lease` grant 的成员执行：

```text
viva tools computer release-lease --task <lane 中的 task_id> \
  --acquired-at <lane 中的 acquired_at> --member <member-id> \
  --grant <grant-id> --reason "已核实旧动作停止；说明核查依据" --confirm
```

只有 PID 与标记都为 NULL 且取得时间未变的行可释放；活 PID 或行已变化均拒绝。删除与 append-only Viva event 同事务提交，事件记录**成员/授权来源**和“已停止”为调用者断言，**不伪装成系统核验或 Haisu 亲自操作**。`--confirm` 本身不是身份认证；需事先由 owner 授予该成员针对这个 task 的 grant，且授权/核实过程仍由操作者负责。无合法 task ID、无法确认原动作已停、或无法取得授权时保持锁定，不应盲目改 SQLite。CLI 的同用户进程隔离不是 OS 沙箱；真实键鼠/焦点目标平台验收仍待完成。本轮仅临时库测试，未释放真实 Viva 租约。

## 第四轮独立 QA 后续整改（待独立复验）

- M-A：租约探针以 `kill(pid,0)` 的 errno 区分 ESRCH 与 EPERM；后者及其他不可判定结果不删除租约。出生标记确认已复用时仍可回收。无 PID 的旧两列表租约无法证明死亡，**不会自动清除**；操作者须先确认原前台动作已停，再人工处理该租约，不得在活持有者期间抢道。
- tools_computer v2 已登记不能重写：新增 v3 条件迁移，把旧两列表经建新表→复制→替换升级成四列，保留 task_id/acquired_at；已有健康四列保持原行和 PID 标记。临时文件库模拟“已登记 v2 但旧表仍两列”升级、重开、持有者不被抢占。
- M-B：`viva data export` 是资产保全用的**原样未脱敏**导出；manifest 显式 `redacted:false`、`contains_sensitive_data:true` 并显示警告，Unix 上新建 0700 目录、0600 文件，拒绝覆盖。导出产物不得提交或分享；标记与私有权限不等于脱敏。
- R4-N1 冲突处理：maintenance v2 的历史 UPDATE 不改写（已登记）。若裸键 X 和 X:0000 并存，迁移保留数据并给出备份/诊断提示，而不是静默丢弃人审决定。**操作者路径**：先停止 Viva，使用 SQLite backup（包括 WAL 中尚未 checkpoint 的更改）保全数据库；可通过 `viva data export --out <仅本人可访问的目录>` 查看原始提案（注意是敏感原文），核对冲突行的 proposal_id、status、resolution_note，再由 owner 决定哪条承载该主题代际零。若两条都要保留，可在备份后对裸键的特定 proposal_id 在 SQLite 事务内改名为 `X:legacy:<proposal_id>`（迁移会再加 `:0000`），保留原状态与主键；重开 `viva doctor` 验证。不要自动合并或删除人审记录。此为人工处置指南，**没有对用户真实库执行**。
- #25 的真实前台输入/目标平台并发验收仍未进行；本轮的安全机制及文件库回归不等于产品判据通过。

## 第三轮独立 QA 后续整改（2026-09-29，待独立复验）

- R3-N8：`start-run` / `resume --cwd` 共用 tracked-tree 干净树检查，resume 必须给 cwd；临时仓库 CLI 回归覆盖 dirty、缺 cwd、提交后恢复。仍有 Git status 与 rev-parse 之间的 TOCTOU；untracked/ignored 文件不纳入 head 证明。
- R3-N9：dispatch 类 workflow pass 的 grant 必须显式绑定本 run 的 task；无 task 的全局 grant 不能充当 per-task 授权。全局 grant 对其他动作的既有语义不变。
- R3-N10：跨进程 SQLite 租约按 OS 进程出生标记核对 PID；旧无标记行仅在 PID 消失或当前进程的运行时长短于该行已持有时长（加两秒容差，说明 PID 被回收）时回收；健康持有者不会仅因超过时限而被抢占。macOS `ps lstart` 精度为秒，极端同秒 PID 回收/系统探针异常仍需目标平台验证。现有 smoke 全是 `foreground:false`，无真实键鼠/焦点生产 spec；#25 前台互斥只能称单机测试机制，**不能称已做目标平台端到端验收**。
- R3-N11：maintenance v2 迁移给旧裸 dedup key 补 `:0000`，保留 proposal 主键、状态、人审结论；已带代际后缀的库保持原样。用从 v1 升级的临时数据库验算。
- R3-N13：设置 VIVA_HOME 而不设置 VIVA_MEMORY_DB 时，provider 路径默认落在 VIVA_HOME 内；没有 VIVA_HOME 的日常默认仍是用户 Hermes 库。临时 home CLI status 回归验证路径，未对真实记忆库写入。
- 仍未修：B5/R3-N12 外部 fact 采纳无 provider 存在性核验、B4 session host_pid 生命周期、B6 停用技能路径分叉及 head TOCTOU；不能将 link 视为已验证的事实存在证明。
- N1：已推送的 **1d0ca4c 与 6f2a4e4** 均包含机器私有应用清单；当前工作树证据已去除清单，但远端 Git 历史 blob 仍可访问。这里只披露 commit 标识，不复制应用名或原文；是否改写历史或接受残留由 owner 决定，本补丁不 force-push。
- QA 自行披露的两处真实库写入：先前 `viva doctor` 给真实 `~/.viva/office.db` 应用了空表 migration；QA 的 `remember` 探针在真实 `~/.hermes/memory_store.db` 创建了 fact_id=28。**本次未操作这两处数据**。是否 archive fact 28 或清理空表，待 owner 精确授权；不可把 VIVA_HOME 隔离补丁说成已消除既有污染。

验收日期：2026-09-29（第二轮整改后更新）。本记录针对独立 QA 复审（REJECT @ f550171）及
第二轮复审（CLOSED 9 · 4 项 N-finding · B 项披露清单）的整改后状态，
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
| #25 grant 放大（无 task 限定 + 无 principal 比对） | **已修（principal 绑定）**：`evaluate` 比对 `principal_member_id`，非本人 grant 即 `NotPrincipal` 拒绝并记录；**保留全局 grant**（task=None）形态——它是 F02 维护所依赖的 owner 直签形态，principal 绑定后滥用面收敛到持有者本人，不可再冒充他人 |
| #25 lease 进程内 + foreground 无可达路径 | **已修（lease 跨进程）**：租约改为 VIVA_HOME 库内 `foreground_leases` 行（写事务即跨进程门），两个独立连接/进程共享同一文件即互斥；f03 测试以两个连接验证。foreground 输入路径仍只在库层提供（真实键鼠注入需要真实任务 + owner grant 时启用），证据文档如实标注 |
| #26 开箱即死 + status 假就绪 | **已修**：python 未显式配置时优先探测 checkout 自带 venv（实测系统 python 缺 ruamel）；`memory status` 现在真实调 adapter（list round trip）并报告 `hrr: active/degraded`（NumPy 降级可见），不再是文件存在性断言 |
| #26 写侧隔离被偷 | **已修**：link 改为 `(fact_id, member_id)` 唯一（migration v2），Bob 写同内容获得**自己的** link 与来源；exit 双门——owner 条件（只能退自己的 claim）+ live `maintain_knowledge` grant；本机真实复现并落证据 |
| #26 link_fact 零调用者 | **已修**：`viva memory link --fact <id>` 资产引用采纳入口 + 真实证据（carol 采纳后可召回） |
| #26 改写召回/provider 高级接口 | **如实记录边界（不可修）**：FTS 候选门控决定同义改写与 CJK 部分串不召回——这是 provider 机制事实，不伪造；产品选择已写入 README（可考虑 probe/related 预扩展或接受边界）。已暴露 provider 真实 `probe` 实体召回（`viva memory probe`）与 HRR 降级可见性；`contradict/fact_feedback` 留待产品裁决，不冒充已用 |
| B1 无验收证据文档 | **已补**：本文档 + `evidence/f01-f04/*.json`；`viva-transition.md` §6 补四行（组件/已查能力/缺口/结论） |
| B2 f04 默认绿 | **已修**：5 个真实 provider 测试改 `#[ignore = "..."]`——CI 上显示为 ignored（可见、不计 PASS），本机以 `cargo test --test f04_memory -- --ignored` 真跑（5/5 通过），输出即证据 |
| B4 拆 4 PR | **不修，理由**：本任务由仓库 owner 明确指示"每个 issue 单独 commit、4 个 issue 统一提交 PR"，owner 指示优先于 issue 模板；以各 issue 证据回链评论补偿 |

## 第二轮复审处置（2026-09-29，N1–N7 + B 项）

| 发现 | 处置 |
| --- | --- |
| N1（BLOCKER，隐私）| **已处置，历史改写待 owner 裁决**：仓库内证据文件已脱敏（verdict/fingerprint 保留，原文移除，commit 5ba7c7a 前身）；`viva tools computer smoke` 的证据输出永久改为摘要化（redacted + bytes + fingerprint），原始快照只留在本机私有 VIVA_HOME store——泄露路径已结构性关闭。⚠️ 已推送的 git 历史仍含原始转储（GitHub 端 blob），改写/接受由 owner 决定；候选命令已附于 PR。|
| N2 迁移缺陷 | **已修**：`foreground_leases` 改为正规 v2 迁移（`IF NOT EXISTS` 同时保住 3570ae2 衍生库），v1 恢复原样 |
| N3 租约焊死 | **已修**：租约行记录 holder pid，acquire 前清理死进程行（`kill -0` argv 数组探测）；崩溃进程不再永久占用输入道；有测试覆盖（幽灵行自愈 + 活持有者不被清） |
| N4 computer_input 无 scope 放大 | **已修**：engine 层强制 `computer_input` grant 必须等于当前 task——全局例外对该动作不存在承托物（QA 判断正确），无 scope 即拒绝为"不是全机许可证" |
| N5 head 形式锚定 | **已修**：删除自由 `--head` 旗标（head 一律 rev-parse 解析）；start-run 拒绝脏工作树（tracked 未提交变更 = 同 SHA 不同内容）；resume 同样只经 checkout 解析；证据侧短 SHA 按 前缀归一化匹配。首轮 f01 证据即"绑在干净 SHA 上却含 1687 行未提交内容"的反面案例，本轮证据已在**提交后的干净树**上重跑（见下） |
| N6 --ignored 假信号 | **已修**：宏改为 panic——`--ignored` 且无 provider 时是可见 FAIL（实测 5 FAILED），有 provider 时 5/5 真通过 |
| N7 adapter 相对路径 | **已修**：解析顺序 env → 运行中二进制的祖先目录（打包布局）→ 编译期 checkout 路径；实测从 /tmp 运行 `viva memory status` 解析成功且探测为真 |
| B 项（不挡合并）| 快赢已做：smoke 观察标记收紧为内容事实（bundleId/window）；maintenance 评审窗口出错也 end。其余按披露随 PR：重言核验为"机制已修、样例仍只读"；session host_pid 只写（一次性进程，无 daemon）；dedup 老库形状、hidden 四因合一、link 不向 provider 核验、截图不持久——均记录于本轮复审原文，接受为已披露限制 |

第二轮证据全部在**提交后的干净树**（commit `5ba7c7a8…`）上经 CLI 真实重跑：
F01 六步走查（含真实 verify 失败→修复→复验）；F02 提议→执行→归档 + 去重复跑；
F03 双任务 executed 且证据文件经隐私检查（不含任何应用清单）；
F04 隔离/独立 claim/归档/采纳/status 探测（HRR active）全链。

## 历史记录（首轮整改，保留备查）

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
- `cargo test --test f04_memory -- --ignored`：5/5 真实 provider 测试通过（无 checkout 的机器上现在是可见 FAIL，不再是 5 个绿 no-op）。
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

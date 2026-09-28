# V12 最终验收记录 — 资源、响应与故障恢复（issue #21 final）

Status: **部分验收（方法 + 真实运行证据 + 故障场景），平台/模型负载项 pending** · 2026-09-28 · Issue: [#21](https://github.com/zuohaisu/viva/issues/21)

本文是 V12 的最终验收记录。所有数字来自真实运行（机器：Mac17,4 / Apple M5 / arm64 / 32 GB / macOS 25.5.0），原始 JSON 证据在 `docs/validation/evidence/v12-final/`。对照方法与首切片基线见 [v12-baselines.md](v12-baselines.md)。

## 1. 本轮覆盖的产品切片（相比 G1 新增）

G1 时只有 CLI 切片；本轮验收覆盖 V07/V09/V10/V14 交付后的组合切片：

- **活跃 Office 控制面**：真实 Unix domain socket 通道、单活跃宿主、CLI 回调、派发/观察/停止。
- **真实 PTY 终端监督**：每个派发一个真 PTY 进程组、优雅停止与升级杀灭、退出记录。
- **崩溃恢复对账**：真实 `kill -9` 宿主进程后的重启对账。
- **工作台组合事实**：多项目/任务/worktree/终端的真实 git 与进程事实。

## 2. 实测结果

### 2.1 资源与响应（真实采样）

| 指标 | 预算上限 | 实测 | 证据 | 状态 |
| --- | --- | --- | --- | --- |
| CLI 命令响应（doctor，7 次中位） | ≤ 1 s | **3.7 ms** | launch.json | within |
| 单 host 进程峰值 RSS（CLI 切片） | ≤ 200 MB | **~4.2 MB** | slice-cli.json | within |
| Office host 常驻峰值 RSS（承载 16 终端时） | ≤ 150 MB（新预算，首测） | **8.7 MB** | peak.json | within |
| 16 个被监督子进程的每进程峰值 RSS | — | ~1.2 MB/个（sleep 替身） | peak.json | 记录 |
| 控制面单次派发往返（16 次中位） | ≤ 500 ms（新预算，首测） | **30 ms** | peak.json | within |
| PTY 输入回程（1 字节真 pty） | ≤ 100 ms | **80 µs** | echo.json | within |
| 冷构建（`cargo clean` 后全 workspace） | ≤ 300 s | **15.0 s** | build.json | within |
| 增量构建 | ≤ 30 s | **0.1 s** | build.json | within |

### 2.2 峰值场景（`tools/acceptance/office_workload.py`，真实控制面）

- 一个隔离 `VIVA_HOME`，真实 `viva office start` 宿主；1 成员 + 3 任务 + 3 live grant（真实迁移库种子）。
- **16 个真实被监督 PTY 进程**经真实通道逐个派发（16 个不同 request key，全部 `replayed: false`），峰值采样观测到 **16/16** 存活子进程。
- 停止隔离：停止 2 个终端后 16→14 live，指定 survivor 仍然 live。
- 优雅停机：宿主退出码 0、socket 释放、机器上 0 个属于本次场景的残留进程。

**诚实标注（写进记录本身）**：这 16 个是真实被监督进程、真实控制面资源行为，但 worker 是 `sleep` 替身，**不是**真实 agent CLI 的模型工作负载。记录 `kind: simulated_agents`，不得当作 16-Agent 产品峰值预算结论（见 §4 pending）。

### 2.3 故障验收（真实进程与真实 kill -9）

全部由 `cargo test --workspace`（154 通过 / 0 失败）中的真实场景证明，关键项：

| 场景 | 证明 | 测试 |
| --- | --- | --- |
| 强制退出（kill -9 宿主） | 重启对账记录 `previous_host_crashed` 与 `execution_orphaned`；孤儿执行如实标 `stopped`；不重跑、不杀孤儿进程、不误认 pid | v07_office::crash_restart_reconciles… |
| 完成任务不重放 | 已完成任务再派发被拒（"never executed again"），零新增执行 | 同上 |
| 停止/退出竞态与误杀 | 停一个终端不影响邻居；信号退出以 `-1` 如实入账；stop 纪律（先 TERM 后 KILL、已 reap 不再发信号）由 V05 单元/集成覆盖 | v07/v14/v05 |
| 授权撤回竞态 | revoke 后派发在生效时刻被拒，零执行创建 | v07_office::revoked_grant… |
| 重复启动/请求重放 | 第二宿主拒绝并指认活跃 pid；同 request key 重放不重复启动 | v07_office::second_host…, cli_dispatches… |
| 无活跃 office 时 mutation | 明确拒绝，不起后台 daemon，不凭空造 socket | v07_office::mutation_without… |
| 优雅退出收尾 | owned 终端全部停止，退出 watcher 先 join（每条退出事实先落账）再写交接记录，下次启动无虚假 orphan 记录；socket 释放 | v07_office::graceful_shutdown…、graceful_shutdown_records_every_exit_before_the_handoff |
| 存储/迁移回滚 | 迁移失败无半状态、事务失败回滚、append-only 触发器 | V01 store 单元 |

### 2.4 工作负载达到预算时的行为

排队/拒绝有解释：派发被拒时返回机器可读原因（授权拒绝带 `DenialReason`、完成任务带状态、无宿主带指引），不自动杀其他任务。测试见 §2.3。

## 3. 零越权 / 误杀 / 归属丢失 / 重放 的证据结论

- **零越权**：撤权竞态拒绝（V07 测试）；grant 范围在生效时刻复核且 **grant 主体必须等于派发成员**（v07_office::a_grant_never_serves_a_member_other_than_its_principal）；控制通道经 peer-uid 校验 + 0700 目录双重门禁（v07_office::host_home_and_channel_are_private_and_authenticated）；聊天无 grant 时扩展侧拒发派发并提议走授权（V09 TS 测试）。
- **零误杀**：停止只发本终端进程组；崩溃对账明确"不 signal 孤儿"；pid start marker 防 pid 复用误认（V05/V07）。
- **零归属丢失**：终端 owner 类型化（member_execution 必带 execution id，DB CHECK 背书）；派发留 launch spec + intent + 归属快照。
- **零重放**：request key 幂等且**按任务隔离**（跨任务同 key 直接拒绝，v07_office::request_keys_never_replay_across_tasks）；task handoff 与 conversation handoff 各自幂等（同一交接登记两次只有一行，conversations 单测）。

## 4. Pending（非 PASS，缺硬件/凭证/真实负载如实标注）

以下项**未验收**，不能由本轮任何模拟结果代替：

1. **16 真实 Agent（模型 CLI）同负载峰值采样** — 需要 Pi/其他 CLI 安装与模型凭证；本轮 sleep 替身只证明控制面资源行为。
2. **Intel macOS 基线与运行验收** — 无 Intel 硬件。
3. **真实 Pi 独立运行 vs Viva 承载同负载的增量对照** — 依赖真实 Pi 会话（凭证未配置）。
4. **V14 三真实任务端到端用户演示（含真实 Pi + 另一已安装 CLI）** — V14 的组合行为已由真实 git/PTY/进程测试覆盖，但"真实 Pi + 另一 CLI"的人工并行开发演示待凭证与工具就绪后按 [first-usable-version.md](../product/first-usable-version.md) 执行。
5. **共享桌面动作的串行验证**（issue 要求）— 未执行。

## 5. 方法可重复性

```bash
cargo build --workspace
python tools/acceptance/sampler.py sample --binary ./target/debug/viva \
    --home /tmp/viva-v12/home --window /tmp/viva-v12/window.lock --out record.json
python tools/acceptance/office_workload.py --binary ./target/debug/viva \
    --home /tmp/viva-peak --window /tmp/viva-window.lock --out peak.json
cargo test --workspace
```

采样窗口 flock 独占；宿主/子树/整机分别采样；RSS 峰值由 OS 报告、从不跨进程相加。`tests/acceptance/`（11 项 pytest）对采样器与场景本身做回归保护。

# V12 G1 — 资源/响应采样方法与首切片基线（issue #21）

Status: **G1 方法可运行 + 首切片真实基线（macOS Apple Silicon）** · 2026-09-28 · Issue: [#21](https://github.com/zuohaisu/viva/issues/21)

本文是 V12 的 G1 门槛交付：可重复的采样方法 + 在首个可用切片上的基线/预算记录。它**不是**整个 V12 的最终验收；Intel Mac、16 真实 Agent 峰值负载、Pi/Viva 增量对照等项保持 pending（见 §6）。

## 1. 方法（`tools/acceptance/sampler.py`）

采样器为 stdlib-only Python 脚本，无需安装产品即可在任何 macOS/Linux 主机运行。诚实规则内建在工具里，不靠约定：

- **角色分开采样**：host（viva 二进制）、worker（pi/codex/claude 等 agent CLI）、browser、build（cargo/rustc 等）按 argv[0] 基名分类。整命令行匹配会误标 shell 驱动脚本，已避免。
- **RSS 不相加**：进程 RSS 含共享页。记录里只有每进程峰值（`max_rss_kb`，由 OS 在退出时报告——毫秒级 CLI 进程 ps 快照抓不到）和整机视图（`vm_stat` / `/proc/meminfo`），并强制携带 `note` 说明角色 RSS 不是可加的物理内存。
- **独占窗口**：采样窗口用 `flock` 独占（`ExclusiveWindow`），第二个采样者会得到 `WindowBusy` 错误而不是静默重叠——采样窗口与其他 Agent 的构建不得同时进行。
- **真实/模拟标注**：每条 workload 记录 `kind: real|simulated`。模拟程序只验证控制逻辑，永远不能顶替真实 Agent 的性能证据。
- **缺失 ≠ 达标**：预算比较（`compare_to_budget`）对没有测到的指标报 `missing`，绝不报 `within`。

命令：

```bash
python tools/acceptance/sampler.py profile
python tools/acceptance/sampler.py sample --binary ./target/debug/viva \
    --home /tmp/viva-v12/home --window /tmp/viva-v12/window.lock --out record.json
python tools/acceptance/sampler.py launch --repeats 7 -- ./target/debug/viva doctor
python tools/acceptance/sampler.py build-times --repo .
```

## 2. 首切片基线（真实测量，2026-09-28）

机器：Mac17,4 / Apple M5 (arm64) / 32 GB / macOS 25.5.0 / 10 CPU。二进制：`target/debug/viva`（V01–V08/V11 代码基线）。切片：`slice-cli`（对隔离 VIVA_HOME 的 init → event add → doctor）。

| 指标 | 测量值 | 方法 |
| --- | --- | --- |
| `viva doctor` 响应（中位，7 次） | **2.6 ms** | 真实进程 wall-clock |
| `viva init` 峰值 RSS | 3392 KiB | OS 退出时报告（`/usr/bin/time -l`） |
| `viva event add` 峰值 RSS | 3552 KiB | 同上 |
| `viva doctor` 峰值 RSS | 3440 KiB | 同上 |
| PTY 输入回程（11 字节，真 pty + `cat`） | **90 µs**，未超时 | 真实 PTY 设备往返 |
| 冷构建（`cargo clean` 后 build） | **12.7 s** | 独占窗口内 |
| 增量构建 | **0.1 s** | 同上 |

原始证据：`docs/validation/evidence/v12-g1/{record,launch,echo,build}.json`。每条均为真实运行；工作负载 `kind: real`。

## 3. 预算（首切片档）

预算按切片分档；未覆盖切片在测到之前没有预算结论。

| 指标 | 预算上限 | 首切片实测 | 状态 |
| --- | --- | --- | --- |
| CLI 命令响应（doctor 级） | ≤ 1 s | 2.6 ms | within |
| 单 host 进程峰值 RSS（CLI 切片） | ≤ 200 MB | ~3.4 MB | within |
| PTY 输入回程 | ≤ 100 ms | 90 µs | within |
| 冷构建 | ≤ 300 s | 12.7 s | within |
| 增量构建 | ≤ 30 s | 0.1 s | within |
| TUI 空闲 CPU（viva office 运行时） | ≤ 5% | 未测（切片未含 TUI 常驻） | missing |
| 16 真实 Agent 同负载整机内存/swap | 待定（需 16-agent 采样） | 未测 | missing |
| Pi 承载 vs 独立运行增量 | 待定 | 未测 | missing |
| Intel Mac 对应基线 | 待定 | 未测（无 Intel 硬件） | pending |

预算是记录不是承诺：超出预算时按 issue 要求排队/拒绝并给解释，不自动杀其他任务。

## 4. 故障验收范围（G1 只列方法，最终证据在 V12 final）

故障场景由同一采样器框架扩展驱动，每项须留下失败或通过的事实记录：大输出/慢磁盘（磁盘日志截断标志可见）、stop/exit 竞态（V05 再收纪律已有单元证明）、强制退出、错误身份、撤回授权竞态、存储回滚（迁移/事务已测）、备份恢复。模拟只验证控制逻辑；16 真实 Agent 峰值与平台结论必须来自真实采样。

## 5. 与其他 issue 的边界

- 本文档/采样器归 V12；不持有产品运行时目录。
- 控制器与进程树分别测（`descendants()` 按 ppid 树遍历）；浏览器/构建进程单独分类，不混入产品内存结论。
- 对真实 Pi 独立运行与 Viva 承载同负载的增量对照：待 V09 交付真实 Pi 承载后在 V12 final 采样。

## 6. Pending（非 PASS，缺硬件/凭证/依赖如实标注）

- **Intel macOS 基线**：无 Intel 硬件，未验收。方法可直接复用（采样器跨平台）。
- **16 真实 Agent 峰值采样**：依赖 V07/V09/V14 集成后才能承载同负载；当前记录不能代替。
- **Pi 独立 vs Viva 承载增量对照**：依赖 V09。
- **TUI 常驻运行时采样**：依赖 V06+V07 组合成可常驻进程。
- **CI 构建时间**：本轮为本地采样；CI runner 时间另由发布流程记录（V13）。

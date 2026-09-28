# V13 — Python 入口退役与行为映射（issue #22）

Status: **已退役（Rust 是唯一 `viva` 入口）** · 2026-09-28

## 1. 退役了什么

- `src/viva/`（46 个文件的 Python 产品运行时）与 `tests/viva/`（20 个文件的旧测试实现）：删除。
- `pyproject.toml` 的 `[project.scripts] viva` 安装入口与 `textual` 运行时依赖：随包删除；`pytest.ini` 保留为验收工具的测试配置。
- **退役不是删绿**：每一条被退役行为的证明都在 Rust 侧存在并有测试（映射见 §2）。CI 的 Python job 只保留 `tests/acceptance`（stdlib-only 验收工具），不再安装产品。

## 2. 行为映射（Python 模块 → Rust 实证）

| Python 模块（退役） | 承载行为的 Rust 位置 | 测试实证 |
| --- | --- | --- |
| `core/paths.py`（VIVA_HOME/0700） | `foundation/paths.rs` | g0_foundation |
| `core/ids.py`（typed ids/RFC3339） | `foundation/ids.rs` | g0_foundation |
| `core/store.py`（原子存储） | `foundation/store.rs`（SQLite WAL/迁移/事务） | store 单元 + 各域重开测试 |
| `core/errors.py` | `foundation/error.rs` | 全套 |
| `core/redaction.py` | `redaction/mod.rs` + 终端磁盘日志字节级脱敏 | v04_authority_redaction |
| `permissions/authority.py`、`grants.py`（READ/PROPOSE/ACT_*、protected actions） | `authority/mod.rs` | v04_authority_redaction |
| `residents/*`（成员/角色/引擎绑定） | `members/mod.rs` | v02_members_projects |
| `workspaces/`、`projects/` | `workspaces/mod.rs`、`projects/mod.rs` | v02_members_projects |
| `tasks/registry.py`、`brief.py` | `tasks/mod.rs`（意图协议、结果、简报） | v03_tasks_history |
| `executions/*` | `foundation/records.rs`（执行/归属/完成需证据）+ `tasks/mod.rs` | v03 + v07 |
| `office/control.py`（控制面） | `office/`（UDS 通道、单活跃宿主、恢复对账） | v07_office |
| `workers/*` | 宿主签发的 channel/credential（`envelope.rs`、`authority/mod.rs`） | g0 + v04 |
| `worktrees/*` | `git/worktrees.rs`（发现/创建/adopt/release 仅记录） | v08_git_github |
| `github/client.py` | `git/evidence.rs`、`git/cli.rs`（只读白名单） | v08_git_github |
| `knowledge/*` | `knowledge/mod.rs` | v11_knowledge |
| `tui/app.py`（Textual） | `tui/mod.rs`（Ratatui）+ `tui/workbench/`、`tui/conversations/` | tui 单元 + v14_workbench |
| `runtime/state.py` | 事件与记录层（`foundation/events.rs`、`records.rs`） | g0 |
| `experience/journal.py` | 如实未迁移：经验/日记能力首版未建，Rust 侧无对应物也无假声明（见 vision 的 honesty 规则） | — |

## 3. 数据资产与边界

- `viva data export --out <dir>`：以 **READ-ONLY** 连接把本 `VIVA_HOME` 的全部事实表导出为 JSON + manifest（测试：v13_release::clean_home_boot_and_data_export_roundtrip）。
- 历史 Ticket Autopilot 数据（`~/.ticket-autopilot/`、仓库内 `qa-verdict.json`、`tasks/` 归档）：**原样保留，未触碰**（ADR 0008 边界继续有效）。
- 他人/既有 worktree：导出与退役都不读写任何 worktree；worktree 管理仍归 V08 域且删除永远需要人类点名授权。

## 4. 发行（Rust 成为产品）

- CI 拆分：`rust.yml`（fmt/clippy/test）+ `ci.yml`（仅验收工具 pytest）+ `release.yml`（tag 触发，macOS arm64/Intel 双产物 + 真实启动 smoke + tar.gz/sha256 + GitHub Release）。
- 干净机器安装：解包 release 产物即可运行 `./viva`（无 Rust 工具链要求）；或 `cargo install --path crates/viva`。
- 外部依赖如实声明（README §安装）：git（必需，worktree/事实查询）、gh（可选，GitHub 证据）、Pi（可选，成员对话宿主）、模型 CLI 凭证（自备，Viva 不存储凭证）。

## 5. Pending（非 PASS）

- release.yml 的真实构建与产物验证：首次 tag 推送时才在真实 runner 上执行；本分支未推 tag，**发行流水线本身未实跑**。
- Intel macOS 运行验收：无硬件，保持 pending（与 V12 一致）。

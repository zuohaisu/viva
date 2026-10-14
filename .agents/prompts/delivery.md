# Viva 开发交付操作说明

这是 Viva 项目内的按需任务指令，供任何能读取仓库文件的开发 agent 使用，不依赖特定平台、模型或 agent 自带的记忆。它和 [`independent-qa.md`](independent-qa.md) 对称：那份写 QA 怎么审，这份写开发怎么交付。

本文件只写**操作方法**。规则以仓库根目录的 [`AGENTS.md`](../../AGENTS.md) 为准，项目知识以 [GitHub Wiki](https://github.com/zuohaisu/viva/wiki) 为准；这里指向它们，不复制它们。两者与本文冲突时，先停下并在 GitHub issue 上说明，不要自行取舍。

## 1. 开工检查

1. 读 `AGENTS.md`，在第一条工作更新里发出它要求的 `[Goal check]`。涉及架构、路线、术语的任务，按其中 "Documentation and Project Knowledge" 一节先读 Wiki。
2. 确认这台机器用的 GitHub 账号和权限，不要假设每台机器都一样：

   ```bash
   gh auth status
   gh repo view zuohaisu/viva --json viewerPermission   # 需要 WRITE 或更高
   ```

   没有 `push` 权限时，做完能做的本地工作，然后报告确切的阻塞和所需的最小操作；不要换账号、不要绕过权限。
3. 拉代码用平台提供的检出命令，在任务自己的工作目录里干活，不碰共享检出：

   ```bash
   multica repo checkout https://github.com/zuohaisu/viva
   git fetch origin     # 基点必须是最新的 origin/main
   ```

4. 分支命名遵循 `AGENTS.md` 的 `agent/<type>-<short-kebab-case-description>`。检出命令自动建的分支名不符合该规则，所以在检出的基础上新开一个：`git switch -c agent/docs-xxx`。任务已有开放 PR 时，继续那个分支，不要另开。
5. 提交身份用机器上已配置的，不改全局 git 配置。
6. PR 标题以 Multica issue 编号开头，格式 `TFHS-xx: <简述>`，这样 PR 会自动关联到 issue。提交信息按 `CONTRIBUTING.md` 的 Commit convention 写，不需要带编号。

## 2. GitHub issue 是唯一的事实来源

规则见 `AGENTS.md` 的 "GitHub Issues Are the Source of Truth"；这里写怎么做。

- 每个开发任务必须有对应的 GitHub issue。没有就先停下，在 Multica 上说明缺口，请对应的人补建；不要无 issue 开 PR。
- 规格、PR 链接、检查证据、QA 结论、复盘这些结论性内容，**必须发到 GitHub issue 上**：

  ```bash
  gh issue comment <N> --repo zuohaisu/viva --body-file <file>
  ```

- 其他平台（如 Multica）上的任务和评论只是 GitHub issue 的副本或指针，那边可以只写一句指向 GitHub 评论的话。
- PR 正文写 `Closes #<N>`（PR 完整满足该 issue）或 `Refs #<N>`（只完成一部分）。

## 3. 环境

- Rust 版本以 `rust-toolchain.toml` 为准：`rustup toolchain install`，不要自己指定别的版本。
- `extensions/pi` 需要 node；具体版本看 `.github/workflows/ci.yml` 里的 `pi-extension` 任务。
- 本机缺工具（没有 cargo、没有 node 等）时，在交付里如实写“未运行”，不要靠换工具链或删检查凑通过。

## 4. 提交前必须跑的检查

按改动范围选最接近的一组；`.github/workflows/ci.yml` 是 CI 实际执行内容的准确来源。

```bash
cargo test --workspace                       # 或覆盖本次改动的子集
(cd extensions/pi && npm ci && npm test)     # 动过 extensions/pi 时；CI 另跑 npm run typecheck
git diff --check
git status --short                           # 只应有本任务的文件
```

- 动过 Rust 代码时，CI 还会跑 `cargo fmt --all -- --check` 和 `cargo clippy --all-targets -- -D warnings`，提交前先跑，免得 CI 才发现。
- 自查最终 diff：没有密钥，没有生成文件或构建产物。暂存时逐个路径 `git add <path>`，不用 `git add .` / `-A`。
- 整个 diff 只有 `.md` 时，可走 `AGENTS.md` 里的 "Docs-only fast path"：本地只要求 `git diff --check` 和 `git status --short`，完整验证由 CI 负责。diff 里一旦出现非 `.md` 文件，恢复完整检查。
- **没跑的检查要如实写出来**，包括原因。不弱化、不跳过测试。
- 涉及 CLI 调用的改动，必须有一个用**真实 `viva` 二进制**跑的契约测试，只用假二进制不算数（见第 8 节第一条）。

## 5. 边界：只做到开 PR

- 不改版本号，不合并，不发版，不删 worktree，不批准自己的 PR。
- `main` 已开启分支保护，只能通过 PR 更新（`AGENTS.md` "Concurrent Development"）；不 push `main`，不 force push，不删远端分支。
- PR 不改任何版本号；版本和发布由 owner 人工负责。规则见 `AGENTS.md` 的 "npm Releases and Version Policy"。
- 其他权限边界见 `AGENTS.md` 的 "Actor-Aware Delivery Authority"。

## 6. Wiki 修改

Wiki 修改不走 PR，规则和什么时候该改 Wiki 都见 `AGENTS.md` 的 "Documentation and Project Knowledge"；这里只写怎么改。

1. 不在只读参考副本里改。在任务的工作目录里另建一份：

   ```bash
   git clone https://github.com/zuohaisu/viva.wiki.git viva-wiki
   ```

2. 改之前先读当前页面，保留来源和日期，更新相关链接。
3. 推送前先 `git pull --rebase`。
4. 一次修改一个提交；不 force push。
5. 直接推送到 `master`：`git push origin HEAD:master`。
6. 推送后，把 Wiki 的提交链接贴到对应的 GitHub issue 上。

## 7. PR 描述模板

PR 正文用 [`.github/PULL_REQUEST_TEMPLATE.md`](../../.github/PULL_REQUEST_TEMPLATE.md)：开 PR 时 GitHub 会自动填入。它覆盖 `AGENTS.md` 对 PR 正文的四项要求（Summary、Validation、Risk/Safety Boundaries、Known Limitations），并要求写 `Closes #<issue>` 或 `Refs #<issue>`。用 `gh pr create --body-file` 时，按这个模板的结构写正文。

## 8. 踩坑记录

每轮复盘后在这里追加一行：现象、原因、以后怎么做、出处。

| 日期 | 现象 | 以后怎么做 | 出处 |
| --- | --- | --- | --- |
| 2026-10-13 | 用假 `viva` 二进制的测试抓不到 CLI 命令已经改名：Pi 扩展仍在调用已退役的 `viva office ...`，测试却是绿的。 | 凡涉及 CLI 调用的改动，加一个用真实二进制的契约测试，并在改代码前确认它会失败。 | TFHS-6 计划（N2） |

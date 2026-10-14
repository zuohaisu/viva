# S2 terminal presentation compare harness

Development aid for [#77](https://github.com/zuohaisu/viva/issues/77) slice **S2 (presentation)**. It checks that Viva’s styled terminal snapshot matches an independent tmux replay of the same PTY byte stream at the same geometry.

This is **not** the acceptance matrix (real Pi TUI, workbench pane, visual QA). Those stay on the issue checklist and QA machines.

## Requirements

- Built `viva` binary (`cargo build --release -p viva`)
- `tmux`
- Throwaway `VIVA_HOME` under `/tmp` (the tool creates one if unset; long paths break AF_UNIX)

## Run

```bash
./tooling/s2-presentation-compare/run-local.sh
```

Or one scenario:

```bash
export VIVA_HOME=$(mktemp -d /tmp/vs2.XXXX)
python3 tooling/s2-presentation-compare/compare.py \
  --viva target/release/viva \
  --cwd "$PWD" \
  --cols 100 --rows 30 --wait 9 \
  -- tooling/s2-presentation-compare/synthetic.sh
```

Exit code `0` means no cell/cursor/alternate-screen diff vs tmux for that scenario.

## What is already covered in CI

Rust regression tests in `crates/viva/tests/m1_terminal.rs` (alternate screen, styled handoff replay, host scrollback viewport, resize replies) run on every `cargo test`. This harness adds a **cross-emulator** check when you have tmux locally.

## Real CLI notes

- **Codex** `exec` must run with `--cwd` pointing at a trusted git checkout (Codex refuses `/tmp`).
- **Pi** is not run here; bind evidence to the same commit on the QA machine per issue #77.
- Interactive TUI splash screens are timing-sensitive; record manual evidence separately.

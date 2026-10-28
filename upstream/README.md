# herdr source baseline

This is the starting source tree for Viva's source-based workbench replacement
(issue [#187](https://github.com/zuohaisu/viva/issues/187)), not another reference
implementation to translate. `herdr/` is an **unmodified, complete Git snapshot**
of the owner's local herdr checkout. Its source, tests, build configuration,
vendored dependencies, docs, and license notices are retained together so that
its original behavior can be built and compared before Viva-specific changes.
Upstream docs are third-party source assets, not Viva's canonical knowledge base.
The source commit and tree are recorded in `herdr-source.json`.

## Build and test the original baseline

```sh
cd upstream/herdr
rustup toolchain install
# Install Zig 0.16.0; alternatively set ZIG to its executable.
cargo build --locked
cargo test --workspace --locked
./target/debug/herdr --version
./target/debug/herdr --help
```

Upstream's broader check is `just check` (requires just, cargo-nextest, Bun,
and the Windows cross-compilation prerequisites described in its `AGENTS.md`).
The direct Cargo commands above do not replace those maintenance, integration,
documentation, or Windows checks.

The nested Cargo workspace intentionally keeps its upstream lockfile, dependency
patches, and Rust toolchain. Run Cargo **from that directory**, not via the root
Viva workspace. Root `cargo test --workspace` and current Viva CI do not test
this nested workspace. Neither workspace's package versions have been changed.

## Compare the import with its source

After the import is committed, this command must print the `tree` recorded in
`herdr-source.json` (all imported bytes, executable bits and symlinks):

```sh
git rev-parse HEAD:upstream/herdr
```

This check establishes the original import, not a prohibition on developing
from it. Future modifications must retain upstream attribution and prominently
mark changed files, as required by Apache-2.0. Keep the original source commit
available as the comparison baseline; document each Viva-specific change.

## Safety and scope

This first slice does **not** replace the installed `viva` entry or migrate any
user data. The binary still identifies as herdr and has herdr's upstream paths,
update channels, socket API, hooks, and worktree actions. It does **not** enforce
Viva's live grants or write Viva Resident/Task/Execution attribution. Do not use
it as a Viva-authorized worker host, attach it to an existing herdr server, run
its updater/installers, or operate existing worktrees as part of baseline tests.
Any interactive test must use a disposable, explicitly isolated session and
configuration, with the exact scope recorded on #187.

No scripts, hooks or workflows from the nested snapshot are installed or
activated in Viva. In particular, its nested `.github/workflows` do not run as
Viva workflows. Before replacing the default Viva runtime, validate authority,
source attribution, redaction, data preservation and real TUI interactions.
Passing upstream tests alone is not evidence that Viva integration is complete.

The current route, decision supersession and remaining acceptance live in the
[Wiki Roadmap](https://github.com/zuohaisu/viva/wiki/Roadmap) and #187.

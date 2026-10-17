# npm distribution (`@zuohaisu/viva`)

The unscoped npm name `viva` is a dead 2014 package, so the product ships
under the **owner scope** `@zuohaisu/viva`. The package name is only the
delivery channel: the installed command stays `viva`.

```text
@zuohaisu/viva              wrapper: bin/viva.js resolves + execs the binary
├── @zuohaisu/viva-darwin-arm64   macOS Apple Silicon binary (os/cpu guarded)
└── @zuohaisu/viva-darwin-x64     macOS Intel binary (os/cpu guarded)
```

- The wrapper's `bin/viva.js` is dependency-free (Node ≥ 18): it resolves
  `@zuohaisu/viva-darwin-<arch>/bin/viva`, spawns it with inherited stdio,
  forwards argv, exit codes and SIGINT/SIGTERM/SIGHUP. No postinstall
  network access — npm's optionalDependencies mechanism picks the platform
  package, and the `os`/`cpu` fields keep the other one away. It passes its own
  package root as `VIVA_NPM_PACKAGE_ROOT` so the native updater can identify
  this channel instead of replacing a file inside npm's dependency tree.
- One-time maintainer setup (human-only): on npmjs.com as `zuohaisu`,
  create a granular **automation** token with read/write for
  `@zuohaisu/*`, and add it as the repository secret `NPM_TOKEN`.
- A `v*` tag push runs `.github/workflows/release.yml` only: it builds the
  macOS tarballs and creates the GitHub Release. That push does not publish
  npm.
- npm publish is a separate manual run of `.github/workflows/npm-publish.yml`.
  In Actions, choose "Publish npm", select the default branch as the
  workflow source, and enter one existing tag such as `v0.3.9`. The tag
  must already have a GitHub Release. The job checks the release checksums,
  requires the tag version (without the leading `v`) to equal
  `crates/viva/Cargo.toml`, then extracts those released binaries and
  re-stages the packages via `prepare-packages.py`. Platform packages go
  out first and the wrapper last. A missing `NPM_TOKEN`, a missing Release,
  a version mismatch, or an npm version that already exists fails the run.
  Nothing is published in those cases, and an existing version is not
  overwritten.

## Updating

`viva update --check` queries the npm `latest` dist-tag. `viva update` upgrades
global installations using `npm install --global --prefix <original-prefix>
@zuohaisu/viva@<checked-version> --no-audit --no-fund --ignore-scripts`, then
verifies the installed wrapper's `--version`. It requires npm on PATH and
write access to the original prefix; it never uses sudo or changes npm's
configuration. npm owns package installation/failure behavior (this is not
an atomic binary-only swap). Local installs are checkable, but updates must
be made explicitly with `npm install @zuohaisu/viva@latest` in their project.

After installing, `viva update` hands a running resident server to the fresh
wrapper through the live handoff — the terminals survive and the old host
exits; reconnect the TUI afterwards. `--no-restart` skips the restart
(`viva server-restart` does it later). With no server running the next
`viva` start simply uses the new version.

## Local checks (no registry, no token)

```bash
packaging/npm/test-local.sh    # stub binary through stage → pack → install → exec
```

`npm install <local dir>` symlinks directories, so the test packs tarballs
first — the same shape a registry install produces.

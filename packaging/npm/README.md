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
  package, and the `os`/`cpu` fields keep the other one away.
- One-time maintainer setup (human-only): on npmjs.com as `zuohaisu`,
  create a granular **automation** token with read/write for
  `@zuohaisu/*`, and add it as the repository secret `NPM_TOKEN`.
- Publishing happens only on tag pushes (`v*`): the release workflow's
  `npm-publish` job extracts the binaries from the released tarballs
  (npm ships exactly the released bits), re-stages the packages via
  `prepare-packages.py` with the tag version, then publishes platform
  packages first and the wrapper last. Without `NPM_TOKEN` the job skips
  with an honest note.

## Local checks (no registry, no token)

```bash
packaging/npm/test-local.sh    # stub binary through stage → pack → install → exec
```

`npm install <local dir>` symlinks directories, so the test packs tarballs
first — the same shape a registry install produces.

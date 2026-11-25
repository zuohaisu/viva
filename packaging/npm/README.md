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
  re-stages the packages via `prepare-packages.py`. Before publishing, it
  packs all three packages and checks that both platform tarballs contain
  the Release's notice/attribution files byte-for-byte. It publishes those
  checked archives, with the wrapper last. A missing `NPM_TOKEN`, a missing Release,
  a version mismatch, or an npm version that already exists fails the run.
  Nothing is published in those cases, and an existing version is not
  overwritten.

## Source versus published packages

The public `latest` tags currently resolve to `0.3.9`, whose binary is the
legacy Viva workbench, not the embedded Herdr build on main. The internal
Tailnet `0.3.10` is a separate, owner-authorized channel; it is not an npm
release and predates PR #202's unified native commands. Check the
[Wiki Releases](https://github.com/zuohaisu/viva/wiki/Releases) for source and
acceptance provenance, not only `viva --version`.

The current release workflow builds with `--features herdr-runtime`. Changing
that workflow does not change old packages. The Release archive includes
`LICENSE`, `NOTICE`, `SBOM.txt`, `THIRD_PARTY_NOTICES.md`, and `licenses/`,
with a source-commit URL. The npm platform packages copy them directly from
the corresponding extracted archives. Missing required materials stop staging.
The `SBOM.txt` is a component summary. `licenses/dependency-notices.json`
contains discovered notice texts for a conservative superset of current Cargo
normal/build inputs from locked, target-filtered workspace metadata and selected
Rust/Zig toolchain notices. It may include unused crates and is not a linker
trace or clearance of source obligations; see P4 work on #191.

## Updating

`viva update --check` queries the npm `latest` dist-tag. `viva update` upgrades
global installations using `npm install --global --prefix <original-prefix>
@zuohaisu/viva@<checked-version> --no-audit --no-fund --ignore-scripts`, then
verifies the installed wrapper's `--version`. It requires npm on PATH and
write access to the original prefix; it never uses sudo or changes npm's
configuration. npm owns package installation/failure behavior (this is not
an atomic binary-only swap). Local installs are checkable, but updates must
be made explicitly with `npm install @zuohaisu/viva@latest` in their project.

Package installation and runtime activation are separate. Compatible legacy
hosts can use the legacy updater's S3 live handoff; `--no-restart` skips it.
Starting a native build can import a compatible live legacy session, but it is
a **degraded import**, not proof of an installer-driven upgrade for every old
version. The field-loss boundaries and compatibility matrix are in
[README.md](../../README.md#update).

For a running **native** host, the current updater only reports that the files
were installed while the previous host remains active. Native `server-restart`
is not integrated, and a frontend reconnect does not prove the backend was
upgraded. With no host running, installation does not start one. Full automatic
native activation, client recovery and failure/rollback validation are required
by [#201](https://github.com/zuohaisu/viva/issues/201), not implemented by this
documentation change. Do not stop live sessions on the strength of a package
install result. Stable `viva update` does not consume the Tailnet dev channel.

## Local checks (no registry, no token)

```bash
packaging/npm/test-local.sh             # macOS: stub binary through stage → pack → install → exec
packaging/npm/test-local.sh --pack-only # both Darwin packages, including omitted-notice rejection
```

`npm install <local dir>` symlinks directories, so the test packs tarballs
first — the same shape a registry install produces.

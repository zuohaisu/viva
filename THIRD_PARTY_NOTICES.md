# Third-party source notices

Viva's root MIT license does not replace the licenses of third-party source.
This file records the source import for [#187](https://github.com/zuohaisu/viva/issues/187).
It does not claim upstream endorsement or grant trademark rights.

## herdr

- Upstream: <https://github.com/herdrdev/herdr>
- Owner-provided source fork: <https://github.com/zuohaisu/herdr>
- Commit: `e35f3937b0efe40ec0dab675709c68e1d8e8c9e6`
- Cargo package version: `0.9.3` (the commit, not this version string, identifies the snapshot).
- Location: [`upstream/herdr/`](upstream/herdr/).
- License: **Apache-2.0**, full text retained at [`upstream/herdr/LICENSE`](upstream/herdr/LICENSE).
- Modifications: none; the complete tracked source tree, including original
  copyright, license and attribution statements, is retained. Provenance is in
  [`upstream/herdr-source.json`](upstream/herdr-source.json).
- No root upstream `NOTICE` file exists at this commit. Nested third-party
  notices remain in their original locations, including Windows ConPTY's
  [`LICENSE`](upstream/herdr/packaging/windows/licenses/Microsoft.Windows.Console.ConPTY-LICENSE.txt)
  and [`NOTICE`](upstream/herdr/packaging/windows/licenses/Microsoft.Windows.Console.ConPTY-NOTICE.md).

The snapshot includes these separately licensed vendored sources:

| Component | Attribution / license | Retained source notice |
| --- | --- | --- |
| libghostty-vt | Copyright (c) 2024 Mitchell Hashimoto, Ghostty contributors; MIT | [`vendor/libghostty-vt/LICENSE`](upstream/herdr/vendor/libghostty-vt/LICENSE); source revision in [`vendor/libghostty-vt.vendor.json`](upstream/herdr/vendor/libghostty-vt.vendor.json) |
| portable-pty | Copyright (c) 2018 Wez Furlong; MIT | [`vendor/portable-pty/LICENSE.md`](upstream/herdr/vendor/portable-pty/LICENSE.md); local patch provenance in [`vendor/portable-pty.patches.md`](upstream/herdr/vendor/portable-pty.patches.md) |

Additional embedded notices and file-level licenses are preserved throughout
that tree; this table does not relicense them. This PR imports source only:
it does not redistribute a new binary or change npm release contents. Any later
binary distribution must retain applicable third-party licenses and notices in
the actual package, not just link to this repository.

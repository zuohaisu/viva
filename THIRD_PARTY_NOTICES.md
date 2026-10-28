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
| Ghostty bash/zsh shell integration (file-level exception to Ghostty's MIT) | **GPL-3.0-or-later**, derived in part from Kitty as stated in the original file headers | [`bash/ghostty.bash`](upstream/herdr/vendor/libghostty-vt/src/shell-integration/bash/ghostty.bash), [`zsh/ghostty-integration`](upstream/herdr/vendor/libghostty-vt/src/shell-integration/zsh/ghostty-integration); full license supplied at [`licenses/GPL-3.0-or-later.txt`](licenses/GPL-3.0-or-later.txt) |
| portable-pty | Copyright (c) 2018 Wez Furlong; MIT | [`vendor/portable-pty/LICENSE.md`](upstream/herdr/vendor/portable-pty/LICENSE.md); local patch provenance in [`vendor/portable-pty.patches.md`](upstream/herdr/vendor/portable-pty.patches.md) |

The two GPL-covered scripts above are conveyed as their complete, unmodified
source, under GPL-3.0-or-later, with their original notices intact and the GPL
text supplied alongside this source distribution. Neither Viva's MIT nor
herdr's Apache-2.0 relicenses those scripts. GPLv3 §4 applies to verbatim source
conveyance; §5's aggregation provision does not automatically relicense separate,
independent works. Whether any future combined or binary product creates broader
obligations requires examination of that actual product, not an assumption from
the top-level license. GPL text source: SPDX's
[`GPL-3.0-or-later.txt`](https://github.com/spdx/license-list-data/blob/main/text/GPL-3.0-or-later.txt).

Additional embedded notices and file-level licenses are preserved throughout
that tree; this table does not relicense them. This PR imports source only:
it does not redistribute a new binary or change npm release contents. Any later
binary distribution must retain applicable third-party licenses and notices in
the actual package, not just link to this repository.

## Review status: incomplete — merge/release hold

This initial notice inventory is **not** a complete compliance clearance. Source
publication is itself redistribution; not shipping a binary does not waive
source-distribution obligations. The GPL file-level exception was identified
and its license text added after the original import; the original import alone
was not sufficient evidence of license completeness.

Before merging this baseline, review the complete tracked-source license scope,
including embedded third-party code and assets. In particular, attribution and
permission scope for bundled sounds, artwork, screenshots and sponsor marks
have not been separately established by this audit. Their presence upstream is
not independent proof of clearance. Do not adopt those marks as Viva branding
or ship uncertain assets; establish permission or omit/replace them first.

Before integration or binary distribution, map Cargo and Zig transitive
licenses, generated/embedded resources and actual link/package contents; supply
the required license texts, notices and corresponding source where applicable.
Do not claim all imported files are MIT/Apache-2.0, or that this notice file
alone proves compliance. Unresolved scope or compatibility questions require
qualified legal review, not a blanket assurance from an automated scan.

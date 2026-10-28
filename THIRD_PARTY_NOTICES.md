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
| Ghostty bash/zsh shell integration (file-level exception to Ghostty's MIT) | **GPL-3.0-or-later**, derived in part from Kitty as stated in the original file headers | [`bash/ghostty.bash`](upstream/herdr/vendor/libghostty-vt/src/shell-integration/bash/ghostty.bash), [`zsh/ghostty-integration`](upstream/herdr/vendor/libghostty-vt/src/shell-integration/zsh/ghostty-integration), [`zsh/.zshenv`](upstream/herdr/vendor/libghostty-vt/src/shell-integration/zsh/.zshenv); full license supplied at [`licenses/GPL-3.0-or-later.txt`](licenses/GPL-3.0-or-later.txt) |
| portable-pty | Copyright (c) 2018 Wez Furlong; MIT | [`vendor/portable-pty/LICENSE.md`](upstream/herdr/vendor/portable-pty/LICENSE.md); local patch provenance in [`vendor/portable-pty.patches.md`](upstream/herdr/vendor/portable-pty.patches.md) |

The three GPL-covered scripts above are conveyed as their complete, unmodified
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

## #190 file-level follow-up (2026-10-28)

The read-only inventory in [`research/herdr-190/inventory.json`](research/herdr-190/inventory.json)
scans all 2,945 committed files (including dotfiles) and records 297 Cargo
packages from official `cargo metadata --locked`, plus 22 Zig manifests and
all eight top-level assets. Reproduce with
[`tooling/herdr-audit/inventory.py`](tooling/herdr-audit/inventory.py). Matches
are discovery signals, not license classification; no match does not mean
permission exists. Cargo metadata covers all platforms, not actual linked
objects. No successful binary exists from this validation run.

Further file-level exceptions and obligations identified in this pass:

| Content | Evidence / source-distribution treatment | Binary/package follow-up |
| --- | --- | --- |
| Ghostty `src/shell-integration/zsh/.zshenv` | A **third** GPL-3.0-or-later script, previously omitted from this table; original full source/header retained; covered by the supplied GPL text | Check whether the actual artifact embeds/installs any of these scripts; do not infer aggregation or combined-work scope from a directory name |
| Ghostty `pkg/libintl/libintl.h` | Header explicitly grants **LGPL-2.1-or-later**; full text now supplied at [`licenses/LGPL-2.1-or-later.txt`](licenses/LGPL-2.1-or-later.txt) | `pkg/libintl` describes gettext 0.24; do not assume this header means libintl is linked into the VT-only build. If combined/linked, assess the actual LGPL source/relinking obligations with qualified counsel |
| Ghostty `src/input/keycodes.zig` | Header identifies Chromium's `dom_code_data.inc`; BSD text now supplied at [`licenses/Chromium-BSD-3-Clause.txt`](licenses/Chromium-BSD-3-Clause.txt). Referenced Chromium source bears Copyright 2013 The Chromium Authors; preserve that attribution alongside its license | Reproduce applicable copyright, conditions and disclaimer in package materials if included |
| Ghostty `pkg/simdutf/vendor/simdutf.h` | Contains the full Facebook/Deepmind/NYU/NEC/IDIAP BSD notice (lines 966–1011), not covered merely by Ghostty MIT | Preserve that notice as well as simdutf's own license. Exact amalgamation provenance unresolved: Zig manifest says 5.2.8, embedded version constants say 9.0.0; do not treat manifest version alone as an SBOM |
| Ghostty fontconfig override headers | Keith Packard permission/disclaimer retained in `fontconfig.h`, `fcint.h`, `fclang.h`; `fcconst.h` marks HPND; `fccase.h` separately cites Unicode terms | Retain copyright/permission in supporting documentation; verify the exact Unicode data notice/version, not just fontconfig's umbrella license |
| Ghostty Highway overrides | `abort.cc` marks Apache-2.0 and BSD-3-Clause; `per_target.cc` marks Apache-2.0 | Fetch the fixed dependency and retain its applicable LICENSE/NOTICE and local notices; downloading the build manifest alone is not enough |
| Ghostty stb image/resize, Zig compatibility, Wayland XML | Full notices remain in the stb headers, `src/lib/compat/README.md`, and `vicinae-hotkey-v1.xml` respectively | Copy applicable notices into actual package materials when included; dual-license choice must be recorded, not silently assumed |
| Ghostty `src/terminal/res/rgb.txt` | `x11_color.zig` explicitly identifies X11 rgb data and its separate MIT/X11 license | Confirm original copyright/permission material for the embedded data; Ghostty's copyright alone is not the data attribution |

New license text provenance (retrieved through GitHub's contents API):

- LGPL text: SPDX license-list-data commit `f715b3660bb164c3d0b81cba9ce94e3c558c4048`, `text/LGPL-2.1-or-later.txt`.
- Chromium BSD text: chromium/chromium commit `58d786a70944808688eb8571db6c00fa6566e307`, `LICENSE`; the referenced `ui/events/keycodes/dom/dom_code_data.inc` was also inspected at that commit. This establishes a notice source, not an exact historical revision of Ghostty's adapted table.

### Actual distribution obligations for integration B

B must create and inspect a license payload **inside each actual install/npm
archive**, not only link to this file. At minimum, carry herdr Apache-2.0,
Ghostty/portable-pty MIT notices, the applicable file-level notices above, and
license/copyright texts for every selected Cargo and Zig dependency actually
included. Keep this provenance manifest and prominently identify modified
Apache-covered files. Cargo `OR` expressions allow a documented choice; `AND`
Unicode terms must not be dropped. Current metadata includes Unicode-DFS-2016,
Unicode-3.0, Zlib, WTFPL, BSD, and Apache LLVM-exception alternatives in addition
to MIT/Apache. Platform-specific dependency closure and downloaded Zig sources
still require verification. On Windows retain ConPTY LICENSE and NOTICE and
verify the exact packaged runtime, not just the Rust crate metadata.

If GPL/LGPL material is conveyed, include its applicable license and complete
corresponding source/build/installation or relinking materials as required by
the actual distribution mode. Source availability on GitHub alone is not a
universal substitute. Scope/combination questions need qualified professional
review. No release clearance is asserted here.

### Assets: unresolved, not approved for redistribution

All eight assets have hashes in the inventory: `logo.png`, `logo.svg`,
`og-card.png`, `screenshot.png`, `sounds/done.mp3`, `sounds/request.mp3`, and
`sponsors/terminal-trove.{png,svg}`. No separate source/author/permission record
was found in the tracked asset tree. This is an evidence gap, not a legal
finding that permission is absent. `src/sound.rs:27–28` embeds both MP3s with
`include_bytes!`; disabling playback does **not** remove them from a binary.
Artwork and sponsor marks must not become Viva branding. Obtain provenance and
permission or explicitly omit/replace uncertain content before redistribution;
that decision remains open. The original snapshot is retained for provenance,
not silently edited or presented as cleared. Source publication itself remains
subject to this unresolved review.

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

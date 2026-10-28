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
records a path, SHA-256, kind and Git mode for **every one of the 2,945 committed
blobs** (including dotfiles, binary fixtures and fonts outside `assets/`) in its
schema-2 `files` list. `kind` is a UTF-8/NUL heuristic (or Git symlink mode), not
a rights determination. It also records 297 Cargo packages from official
`cargo metadata --locked`, 22 Zig manifests and the eight **top-level** assets.
Schema 1 at `ed7abd5` read every blob but only retained matched text files,
Zig manifests and top-level assets; it omitted binary fixtures such as Fira Code.
Reproduce with
[`tooling/herdr-audit/inventory.py`](tooling/herdr-audit/inventory.py). Its
`viva_commit` is the running checkout's HEAD and will differ on later commits;
compare `source_tree` and the other payload fields for cross-commit reproduction.
The byte-identical repeat claim applies only to the same recorded HEAD and
Cargo metadata. Run regression checks with
`python3 -m unittest discover -s tooling/herdr-audit -v`; CI's existing
`acceptance-tools` job now runs them against the real committed source tree.
Matches
are discovery signals, not license classification; no match does not mean
permission exists. Cargo metadata covers all platforms, not actual linked
objects. This describes the initial PR #192 run, which had no successful binary;
the subsequent fixed-source runtime evidence is recorded below.

Further file-level exceptions and obligations identified in this pass:

| Content | Evidence / source-distribution treatment | Binary/package follow-up |
| --- | --- | --- |
| Ghostty `src/shell-integration/zsh/.zshenv` | A **third** GPL-3.0-or-later script, previously omitted from this table; original full source/header retained; covered by the supplied GPL text | Check whether the actual artifact embeds/installs any of these scripts; do not infer aggregation or combined-work scope from a directory name |
| Fira Code font in Ghostty `pkg/freetype/res/FiraCode-Regular.ttf` | Font `name` table: **Copyright 2014-2021 The Fira Code Project Authors (https://github.com/tonsky/FiraCode)**, version 6.002, explicitly **SIL Open Font License 1.1**. Full font-specific license/copyright supplied at [`licenses/OFL-1.1.txt`](licenses/OFL-1.1.txt); original font and embedded copyright untouched. Parsed metadata and SHA-256 are in [`font-evidence.json`](research/herdr-190/font-evidence.json) | This file is already redistributed in the source snapshot, independently of whether FreeType is linked. Retain font copyright and OFL text with every conveyed font copy; respect OFL sale/name/endorsement terms. Do not substitute FreeType's license or Ghostty MIT for OFL |
| Ghostty `pkg/libpng/pnglibconf.h` | Header says libpng 1.6.38.git, retains Cosmin Truta and Glenn Randers-Pehrson copyright, and refers to a missing `png.h` license. Supply upstream 1.6.38's complete LICENSE at [`licenses/libpng-LICENSE.txt`](licenses/libpng-LICENSE.txt) (PNG Reference Library License v2 plus historical v1 text) | Manifest separately says 1.6.43. The release prebuilt header differs in its version and copyright-year lines; do not claim it is the exact imported revision. Preserve local copyright headers and verify the fixed downloaded dependency's LICENSE/NOTICE against B's real artifact |
| Ghostty `pkg/libintl/libintl.h` | Header explicitly grants **LGPL-2.1-or-later**; full text now supplied at [`licenses/LGPL-2.1-or-later.txt`](licenses/LGPL-2.1-or-later.txt) | `pkg/libintl` describes gettext 0.24; do not assume this header means libintl is linked into the VT-only build. If combined/linked, assess the actual LGPL source/relinking obligations with qualified counsel |
| Ghostty `src/input/keycodes.zig` | Header identifies Chromium's `dom_code_data.inc`; BSD text now supplied at [`licenses/Chromium-BSD-3-Clause.txt`](licenses/Chromium-BSD-3-Clause.txt). Referenced Chromium source bears Copyright 2013 The Chromium Authors; preserve that attribution alongside its license | Reproduce applicable copyright, conditions and disclaimer in package materials if included |
| Ghostty `pkg/simdutf/vendor/simdutf.h` | Contains the full Facebook/Deepmind/NYU/NEC/IDIAP BSD notice (lines 966–1011), not covered merely by Ghostty MIT. Both h/cpp exactly match Ghostty update `e89cc0b`; the h file also matches fixed Ghostty base `44f2a44df` | Full simdutf MIT and embedded BSD texts now supplied under [`licenses/herdr-native/simdutf/`](licenses/herdr-native/simdutf/). Wrapper still says 5.2.8 and embedded version 9.0.0; official v9 release amalgamations differ beyond generation timestamps. Do not relabel this as an unmodified official release |
| Ghostty fontconfig override headers | Keith Packard permission/disclaimer retained in `fontconfig.h`, `fcint.h`, `fclang.h`; `fcconst.h` marks HPND; `fccase.h` separately cites Unicode terms | Retain copyright/permission in supporting documentation; verify the exact Unicode data notice/version, not just fontconfig's umbrella license |
| Ghostty Highway overrides | `abort.cc` marks Apache-2.0 and BSD-3-Clause; `per_target.cc` marks Apache-2.0 | Fetch the fixed dependency and retain its applicable LICENSE/NOTICE and local notices; downloading the build manifest alone is not enough |
| Ghostty stb image/resize, Zig compatibility, Wayland XML | Full notices remain in the stb headers, `src/lib/compat/README.md`, and `vicinae-hotkey-v1.xml` respectively | Copy applicable notices into actual package materials when included; dual-license choice must be recorded, not silently assumed |
| Ghostty `src/terminal/res/rgb.txt` | `x11_color.zig` explicitly identifies X11 rgb data and its separate MIT/X11 license | Confirm original copyright/permission material for the embedded data; Ghostty's copyright alone is not the data attribution |

New license text provenance (retrieved through GitHub's contents API):

- Fira Code font-specific OFL text: tonsky/FiraCode commit `eee6db993696aba61ff4eef03698e2987d79910c` (tag 6.2), `LICENSE`, including its 2014 copyright. The font's own 2014–2021 copyright above is retained separately; font name-table metadata supplies its licensing declaration, not a claim that it matches the release binary byte-for-byte.
- libpng text: pnggroup/libpng commit `0a158f3506502dfa23edfc42790dfaed82efba17` (v1.6.38), `LICENSE`. `scripts/pnglibconf.h.prebuilt` was compared: version and Cosmin Truta copyright-year lines differ from the imported header; all other lines match. Exact imported prebuilt revision remains unresolved, distinct from supplying the release's full license text.
- LGPL text: SPDX license-list-data commit `f715b3660bb164c3d0b81cba9ce94e3c558c4048`, `text/LGPL-2.1-or-later.txt`.
- Chromium BSD text: chromium/chromium commit `58d786a70944808688eb8571db6c00fa6566e307`, `LICENSE`; the referenced `ui/events/keycodes/dom/dom_code_data.inc` was also inspected at that commit. This establishes a notice source, not an exact historical revision of Ghostty's adapted table.

### Fixed-source runtime and dependency follow-up (2026-10-28, after #192)

The original tree now **builds and runs on Intel macOS 13.7.8**, with Rust 1.96.1
and Zig 0.16.0, without changing any tracked upstream file or dependency pin.
The original source commit was fetched into a separate bare object store; its
tree matches `HEAD:upstream/herdr` exactly. Evidence, binary hash, commands,
initial failures and remaining limits are in
[`completion/baseline.json`](research/herdr-190/completion/baseline.json) and
[`checks.json`](research/herdr-190/completion/checks.json). The binary itself is
not committed or distributed. CLI version/help succeeded; 3,714 upstream Rust
tests passed, with 12 ignored profiles **not executed**. A real isolated PTY
smoke exercised input/Unicode, splits, mouse focus/scroll, resize and
same-process detach/attach. Raw ANSI streams and supporting text projections
are retained; these are not physical screenshots or human visual acceptance.

The download failure was not solved by changing dependencies or evading
Codeberg's IP block. Official Ghostty URLs were fetched with curl and verified
by Zig; an independently accessible GitHub repository contained the exact
translate-c commit and yielded the **same original content hash**. Its fixed
aro dependency was likewise hash-verified. A different bundled translate-c
candidate was rejected, not substituted. Build instructions remain in
[`upstream/README.md`](upstream/README.md).

- [`native-notices.json`](research/herdr-190/completion/native-notices.json)
  indexes captured texts in [`licenses/herdr-native/`](licenses/herdr-native/):
  Highway Apache/BSD, Wuffs MIT/Apache, zlib, CC0 pixel resources, translate-c
  MIT, aro MIT/Unicode, uucode MIT and its supplemental Unicode/width notices,
  plus simdutf MIT/BSD. Some are build-graph inputs rather than proven binary
  contents. The themes archive lacks a root notice in the fetched cache;
  inclusion in any future artifact requires review, not an MIT assumption.
- uucode's Zig `.paths` omits supplemental license files referenced by its
  LICENSE.md. These were recovered from the full fixed archive and verified
  against the original GitHub commit's individual Git blob IDs before copying.
- [`cargo-target-notices.json`](research/herdr-190/completion/cargo-target-notices.json)
  records 155 normal/build packages for `x86_64-apple-darwin`, their declared
  licenses and local notice-file hashes. Four Ratatui Cargo archives omitted
  their workspace license; the full MIT notice was recovered from the exact
  `.cargo_vcs_info.json` revision under `licenses/herdr-native/ratatui/`.
- **Unresolved legal-scope evidence, not agent clearance:** objc2/block2/
  objc2-encode/dispatch2 archives omit a local notice. Their exact upstream
  workspace [`LICENSE.md`](licenses/herdr-native/objc2/LICENSE.md) declares
  MIT/trio alternatives but explicitly questions distribution of Apple-SDK
  derived works. This statement is retained verbatim at its fixed revision;
  qualified professional review is needed for that scope, not a scanner's or
  agent's assurance. It is not a finding that all such use is prohibited.

`just check` completed its Unix lint/tests/maintenance/integration stages in
an isolated HOME with reduced concurrency, then stopped at missing Windows SDK
configuration. No SDK license was accepted or SDK downloaded. A default-thread
attempt's SSH-test timeout is retained as FAIL; its remaining tests were not
run. Separate Unix suite and docs runs do not turn the full check into PASS.
Top-level media rights, remaining embedded data, actual final package notices,
independent QA and human visual acceptance remain open; #191 is not activated.

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

Binary/font/testdata files outside `assets/` now have `files` records as well;
that visibility does not establish their permissions. Fira Code's explicit OFL
notice is addressed above, not silently treated as an MIT/Apache asset. Remaining
fixture and embedded-data provenance still needs review.

The eight **top-level** assets have hashes in the inventory: `logo.png`, `logo.svg`,
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

## Review status: incomplete — integration/release hold

PR #192's partial notice/evidence delivery was merged; that was not completion
of #190 or a distribution clearance. Current follow-up evidence supersedes the
old network-blocked runtime state, not the unresolved licensing/acceptance gates.

This initial notice inventory is **not** a complete compliance clearance. Source
publication is itself redistribution; not shipping a binary does not waive
source-distribution obligations. The GPL file-level exception was identified
and its license text added after the original import; the original import alone
was not sufficient evidence of license completeness.

Before treating #190 as complete or entering #191, review the complete tracked-source license scope,
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

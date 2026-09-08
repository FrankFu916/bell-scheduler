# Third-party software notices

Bell's original source is licensed under [Apache-2.0](LICENSE). Third-party
components keep their own licenses and copyright notices. This file records
the dependency boundary; it does not relicense dependencies or replace the
complete notices required for a binary distribution.

## Locked source dependencies

- Rust versions and checksums are pinned in [Cargo.lock](Cargo.lock).
- Frontend versions, integrity values, and declared licenses are pinned in
  [frontend/package-lock.json](frontend/package-lock.json).
- [The dependency license inventory](third_party/dependency-license-inventory.json)
  lists every locked registry package, including build, test, and optional
  platform dependencies. It records declared license expressions unchanged and
  the lockfile digests from which it was generated. It is not an assertion that
  every listed package is included in every executable.

The dependency set contains more than Apache-2.0/MIT software. For example,
`cssparser`, `cssparser-macros`, `dtoa-short`, `option-ext`, and `selectors`
declare MPL-2.0; Unicode components declare Unicode-3.0; some packages combine
multiple notice obligations. Slash-separated expressions in older metadata
are preserved as declared instead of silently normalized. Use each exact
package's upstream license files and source when preparing a distribution.
The inventory's versioned source links provide access to those packages.

The repository does not vendor the Cargo registry, `node_modules`, or the
official OR-Tools distribution. Downloaded dependencies and generated build
outputs are excluded from the source tree by [.gitignore](.gitignore).

## Google OR-Tools and native dependencies

The C++ worker uses Google OR-Tools **9.15.6755**, upstream tag **v9.15**,
under the [upstream Apache-2.0 license](https://github.com/google/or-tools/blob/v9.15/LICENSE).
The exact official archive URL and SHA-256 are recorded in
[ortools.lock.json](solver/ortools-worker/third_party/ortools.lock.json).
There are no local OR-Tools source patches recorded in that manifest.

The official macOS arm64 archive is a collection of native components. Its
shared libraries include OR-Tools, Protobuf, Abseil, RE2, Coin-OR, HiGHS, SCIP,
SoPlex, and compression libraries. The OR-Tools license alone is not a license
inventory for that complete collection.

The inspected archive provides `share/doc/ortools/LICENSE` and license texts
under `share/licenses/scip/` and `share/licenses/soplex/`, including the bundled
TCLIQUE, tinycthread, fmt, and zstr notices. A native distribution must preserve
these texts and include the matching licenses, copyright notices, and source
references for every additional component actually shipped. The install-tree
manifest and recursive dependency scan determine that set; the source-level
Cargo/npm inventory above does not cover the native archive's contents.

## Distribution records

A source prerelease and a redistributable desktop binary have different
contents. A binary release must carry its complete component notices and
applicable source-access information alongside the actual executable/library
manifest. A local build, development staging tree, or successful solver test
does not establish that this distribution review has been completed.

This alpha distributes source only, without an application bundle, disk image,
or native libraries. The [native component inventory](third_party/native/inventory-v1.json)
records the components, matching notices, source references, and patches used by
the macOS development build. The notice files under [third_party/native](third_party/native)
retain their upstream copyright and license terms.

The inventory currently sets `binaryRedistributionReady` to `false`: matching
source and notice provenance for the CSparse and IPX/BASICLU code embedded in
HiGHS remains unresolved. A successful local build does not change this status.

# r13 system GUI candidate CI

`.github/workflows/candidate-gui.yml` produces a clean system-profile GUI
Flatpak from the exact reviewed source and OpenSnitch gitlink. It does not
produce a standalone bridge tarball: bridge identity belongs to the matching
immutable image and its installed binary/manifest. Candidate publication does
not establish runtime acceptance or authorize stable publication.

The primary must independently obtain resolved source/workflow reviews and
successful native/QML CI at the immutable head. Only then may the primary set
the repository variable `R13_CANDIDATE_REVIEWED_SHA` to that full lowercase SHA
and push that same commit to `r13-candidate`. An absent or different variable
holds both build and publisher. Branch movement invalidates that authorization;
a changed head needs fresh review and green CI. The variable is owner-managed
authorization, not a candidate-authored PASS receipt. Review the workflow and
credential scope before setting it. This implementation has not been executed
on GitHub or accepted for publication.

The new workflow uses a branch push so it can run before any main merge. GitHub
requires default-branch registration for manual-dispatch and workflow-run
triggers; those are not prerequisites here. See the official
[event reference](https://docs.github.com/en/actions/reference/workflows-and-actions/events-that-trigger-workflows)
and [manual-run guide](https://docs.github.com/en/actions/how-tos/manage-workflow-runs/manually-run-a-workflow).
`ci.yml` keeps its native/QML gates, explicitly checks out each PR head SHA with
submodules and no persisted credentials, and also tests candidate pushes.

The read-only builder exports tracked source and the exact pinned submodule
into a fresh directory, rejects developer Cargo configuration and cached
binaries, verifies the pinned Cargo generator before execution and verifies
its generated registry inputs against `Cargo.lock`. A job-local Flatpak
installation records KDE Platform/SDK 6.11 and Rust extension 25.08 commits
before and after a cache-disabled, locked/offline system-profile source build.
SDK/compiler/linker versions, source/archive/tree/gitlink/lock/generator hashes,
manifest and exported permissions, executable hashes, Flatpak commit, bundle
hash, build log and workflow/run/attempt identities form `provenance.json` and
a canonical, hash-bound `candidate-packet.json`. PIE/RELRO/NOW/non-executable
stack, mold linkage, system transport, no network grant and removal of
build-only protoc are checked before export. Native and QML test binaries
never supply exported GUI bytes. See [Flatpak builder reference](https://docs.flatpak.org/en/latest/flatpak-builder-command-reference.html).

A separate fresh publisher job runs only pinned trusted actions and inline
reviewed validation. It downloads the same-run artifact, accepts nine fixed,
bounded regular filenames, hashes every byte, and validates source/tree/
gitlink/manifest/lock/generator identities against immutable GitHub source
responses. It requires the latest returned exact-head native CI run and every
required job in its latest attempt to succeed. Missing, skipped, failed or
still-running jobs hold publication. The publisher never checks out source,
restores caches, unpacks a Flatpak or executes candidate code. The write token
is repository-wide; fixed reviewed publisher code enforces the target boundary.

The publisher attests the validated bundle and packet as data. It creates only
`r13-candidate`, at the reviewed commit, as a draft prerelease. It verifies
server-reported uploaded asset SHA256/size/name and rechecks the tag before
making that prerelease public. The release never becomes latest; an existing
release or mismatched tag is refused. Failure after creation leaves a draft
and HOLD, never overwrites or deletes assets. A rerun of only the publisher
cannot reuse a prior-attempt packet: run/attempt mismatch fails closed. Inspect
partial results rather than bypassing these checks. A full rerun builds a new
candidate which needs fresh artifact/runtime acceptance.

Before runtime testing, the primary verifies trusted CI provenance for the
bundle and packet, independently compares published bytes, then records the
complete image/GUI acceptance packet. Independent review/CI/runtime receipts
must bind that packet hash; fields in this candidate packet cannot authorize
promotion. Stable GUI publication must reuse the accepted bundle bytes and
must not trigger the existing `v*` bridge build-on-tag workflow.

Run offline boundary tests with:

```sh
python3 packaging/flatpak/test-candidate-publication.py
```

They execute the exact publisher scripts with mocked GitHub APIs and assert
refusal before release writes for wrong targets, source/workflow/ref/run/
attempt/profile identities, altered packet/assets/provenance/generator inputs,
stale CI, moved refs, existing releases, extra files, symlinks, hardlinks,
empty and oversized files. Partial uploads and moved tags remain drafts.
These tests establish local boundary behavior, not remote CI or acceptance.

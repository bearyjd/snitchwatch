//! Shape tests for the bridge release pipeline: `.github/workflows/release.yml`,
//! `packaging/release/pins.env` and `packaging/release/build-bridge.sh`.
//!
//! String-based, like `packaging_shape.rs`. These do NOT run the workflow or
//! the container build — that needs GitHub Actions / podman and is covered by
//! `just release-bridge-repro` + `just release-verify`. They assert the
//! load-bearing invariants a careless edit could silently regress: the builder
//! image digest staying in lock-step with `pins.env`, write access staying
//! confined to the publish job (which only tag pushes reach, behind the
//! `release` environment and a tag/release-state guard), releases staying
//! drafts, every action staying pinned to a full commit SHA, and no `run:`
//! script ever interpolating a `${{ }}` expression (script injection).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const WORKFLOW: &str = ".github/workflows/release.yml";
const PINS: &str = "packaging/release/pins.env";
const BUILD_SCRIPT: &str = "packaging/release/build-bridge.sh";

fn workspace_file(rel: &str) -> PathBuf {
    // CARGO_MANIFEST_DIR is crates/snitchwatch-bridge; go up two to the root.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel)
}

fn read(rel: &str) -> String {
    let path = workspace_file(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {}", path.display(), e))
}

fn is_hex(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn unquote(s: &str) -> &str {
    s.strip_prefix('\'')
        .and_then(|s| s.strip_suffix('\''))
        .or_else(|| s.strip_prefix('"').and_then(|s| s.strip_suffix('"')))
        .unwrap_or(s)
}

/// `line` without a trailing ` # comment`.
fn strip_comment(line: &str) -> &str {
    line.split_once(" #")
        .map_or(line, |(code, _)| code)
        .trim_end()
}

/// Non-blank, non-comment lines: original indentation kept, trailing
/// comments removed.
fn code_lines(text: &str) -> Vec<&str> {
    text.lines()
        .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
        .map(strip_comment)
        .collect()
}

/// The workflow's top level: everything before the `jobs:` line.
fn top_level(workflow: &str) -> &str {
    workflow
        .split("\njobs:\n")
        .next()
        .expect("split always yields a first item")
}

/// One job's section: from its `  <name>:` header to the next job header.
fn job(workflow: &str, name: &str) -> String {
    let jobs = workflow
        .split("\njobs:\n")
        .nth(1)
        .expect("release.yml must have a jobs: block");
    let header = format!("  {name}:");
    let mut inside = false;
    let mut section = Vec::new();
    for line in jobs.lines() {
        // Job headers are the only lines indented by exactly two spaces.
        let is_header = line.starts_with("  ")
            && !line[2..].starts_with([' ', '#'])
            && line.trim_end().ends_with(':');
        if is_header {
            inside = line.trim_end() == header;
        }
        if inside {
            section.push(line);
        }
    }
    assert!(!section.is_empty(), "release.yml has no `{name}` job");
    section.join("\n")
}

/// The trimmed child lines of the block opened by `header` (e.g.
/// `permissions:`) within `lines`; empty if there is no such block.
fn block<'a>(lines: &[&'a str], header: &str) -> Vec<&'a str> {
    let indent = |l: &str| l.len() - l.trim_start().len();
    let Some(start) = lines.iter().position(|l| l.trim() == header) else {
        return Vec::new();
    };
    let base = indent(lines[start]);
    lines[start + 1..]
        .iter()
        .take_while(|l| indent(l) > base)
        .map(|l| l.trim())
        .collect()
}

/// Values of the job-level `key:` lines in `lines` (one job's code lines):
/// exactly four spaces of indent, so a step-level `if:` or `timeout-minutes:`
/// can never stand in for the job-level one.
fn job_level<'a>(lines: &[&'a str], key: &str) -> Vec<&'a str> {
    lines
        .iter()
        .filter_map(|l| l.strip_prefix("    ")?.strip_prefix(key))
        .map(str::trim)
        .collect()
}

/// The text of every `run:` value in `workflow`, whether inline, a multi-line
/// scalar or a `run: |` / `run: >-` block: the rest of the `run:` line plus the
/// following lines that are blank or indented deeper than the `run` key.
///
/// Raw lines, deliberately not `code_lines`: the Actions runner expands
/// `${{ }}` over the whole value *before* the shell sees it, shell comments
/// included, so a `${{` in a comment inside a script is just as much an
/// injection as one in a command. A lower-indented line (a YAML comment
/// between steps, the next key) ends the value.
fn run_bodies(workflow: &str) -> Vec<String> {
    let indent = |l: &str| l.len() - l.trim_start().len();
    let lines: Vec<&str> = workflow.lines().collect();
    let mut bodies = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        i += 1;
        // `run:` may follow any number of `- ` sequence markers.
        let mut key = line.trim_start();
        let mut key_col = indent(line);
        while let Some(rest) = key.strip_prefix("- ") {
            key_col += key.len() - rest.trim_start().len();
            key = rest.trim_start();
        }
        let Some(first) = key.strip_prefix("run:") else {
            continue;
        };
        let mut body = first.to_string();
        while i < lines.len() && (lines[i].trim().is_empty() || indent(lines[i]) > key_col) {
            body.push('\n');
            body.push_str(lines[i]);
            i += 1;
        }
        bodies.push(body);
    }
    bodies
}

/// `KEY=value` lines of `pins.env` (comments and blanks ignored).
fn pins() -> BTreeMap<String, String> {
    read(PINS)
        .lines()
        .map(strip_comment)
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| {
            let (key, value) = l
                .split_once('=')
                .unwrap_or_else(|| panic!("{PINS}: not a KEY=value line: `{l}`"));
            (key.trim().to_string(), unquote(value.trim()).to_string())
        })
        .collect()
}

#[test]
fn workflow_triggers_on_version_tags_and_manual_dispatch_only() {
    let body = read(WORKFLOW);
    let top = code_lines(top_level(&body)).join("\n");
    for needle in ["push:", "tags:", "workflow_dispatch:"] {
        assert!(
            top.contains(needle),
            "release.yml `on:` missing `{needle}`\n{top}"
        );
    }
    assert!(
        top.contains("'v*'") || top.contains("\"v*\""),
        "release.yml must trigger on tags matching `v*`\n{top}"
    );
    for forbidden in ["branches:", "pull_request"] {
        assert!(
            !top.contains(forbidden),
            "release.yml must not trigger on `{forbidden}` — only tags and manual dispatch\n{top}"
        );
    }
}

#[test]
fn builder_container_image_matches_pins_env() {
    let pins = pins();
    let pinned = pins
        .get("SW_BUILDER_IMAGE")
        .unwrap_or_else(|| panic!("{PINS} must define SW_BUILDER_IMAGE"));
    assert!(
        pinned.contains("fedora:44@sha256:"),
        "SW_BUILDER_IMAGE must be a digest-pinned Fedora 44 image, got `{pinned}`"
    );
    let digest = pinned.rsplit_once("@sha256:").map_or("", |(_, d)| d);
    assert!(
        is_hex(digest, 64),
        "SW_BUILDER_IMAGE digest must be 64 lowercase hex chars, got `{digest}`"
    );

    let body = read(WORKFLOW);
    let build = job(&body, "build-bridge");
    let images: Vec<&str> = code_lines(&build)
        .into_iter()
        .filter_map(|l| l.trim().strip_prefix("image:"))
        .map(|v| unquote(v.trim()))
        .collect();
    assert_eq!(
        images,
        [pinned.as_str()],
        "build-bridge's `container: image:` must equal SW_BUILDER_IMAGE in {PINS} \
         (bump both together)"
    );
}

#[test]
fn checkout_fetches_submodules_without_persisting_credentials() {
    let body = read(WORKFLOW);
    let build = code_lines(&job(&body, "build-bridge")).join("\n");
    // vendor/opensnitch's ui.proto is compiled by snitchwatch-proto's build.rs.
    assert!(
        build.contains("submodules: true"),
        "build-bridge's checkout needs `submodules: true`\n{build}"
    );
    assert!(
        build.contains("persist-credentials: false"),
        "build-bridge's checkout must set `persist-credentials: false`\n{build}"
    );
}

#[test]
fn write_permissions_exist_only_in_the_publish_job() {
    let body = read(WORKFLOW);
    let all = code_lines(&body);
    let publish_section = job(&body, "publish");
    let publish = code_lines(&publish_section);

    assert_eq!(
        block(&code_lines(top_level(&body)), "permissions:"),
        ["contents: read"],
        "workflow-level permissions must be exactly `contents: read`"
    );

    // `permissions: write-all` (anywhere) is a blanket write grant.
    assert!(
        !all.iter().any(|l| l.contains("write-all")),
        "release.yml must not use `write-all` permissions"
    );

    let writes = |lines: &[&str]| {
        lines
            .iter()
            .filter(|l| l.trim().ends_with(": write"))
            .count()
    };
    let count = |lines: &[&str], needle: &str| lines.iter().filter(|l| l.trim() == needle).count();
    assert_eq!(
        count(&all, "contents: write"),
        1,
        "`contents: write` must appear exactly once in release.yml"
    );
    assert_eq!(
        count(&publish, "contents: write"),
        1,
        "`contents: write` must be in the publish job"
    );
    assert_eq!(
        writes(&all),
        writes(&publish),
        "no `: write` permission may be granted outside the publish job"
    );

    let mut grants = block(&publish, "permissions:");
    grants.sort_unstable();
    assert_eq!(
        grants,
        ["attestations: write", "contents: write", "id-token: write"],
        "publish job needs exactly contents/id-token/attestations write"
    );
}

#[test]
fn publish_runs_only_for_tag_pushes_after_a_successful_build() {
    let body = read(WORKFLOW);
    let publish = code_lines(&job(&body, "publish")).join("\n");
    assert!(
        publish.contains("needs: [build-bridge, audit-bridge]"),
        "publish must `needs: [build-bridge, audit-bridge]` (the build AND the \
         RustSec gate)\n{publish}"
    );
    // A manual run from a tag ref also has ref_type == 'tag'; it must not
    // publish. Exact match, so no `||` (or anything else) can widen it.
    let publish_lines: Vec<&str> = publish.lines().collect();
    let conditions = job_level(&publish_lines, "if:");
    assert_eq!(
        conditions.len(),
        1,
        "publish must have exactly one job-level `if:`: {conditions:?}"
    );
    assert_eq!(
        unquote(conditions[0]),
        "github.event_name == 'push' && github.ref_type == 'tag'",
        "publish `if:` must be exactly the tag-push condition"
    );
}

#[test]
fn build_job_inherits_the_read_only_default_permissions() {
    let body = read(WORKFLOW);
    for name in ["build-bridge", "audit-bridge"] {
        let section = job(&body, name);
        let lines = code_lines(&section);
        // Any form counts: a block, `read-all`, `write-all`, `{}`.
        let overrides: Vec<_> = lines
            .iter()
            .filter(|l| l.trim_start().starts_with("permissions:"))
            .collect();
        assert!(
            overrides.is_empty(),
            "{name} runs repo code and must inherit the workflow's read-only \
             permissions, but sets {overrides:?}"
        );
    }
}

#[test]
fn audit_job_gates_on_the_shipped_dependency_set() {
    let body = read(WORKFLOW);
    let audit = code_lines(&job(&body, "audit-bridge")).join("\n");
    for needle in [
        "cargo install --locked cargo-audit@",
        "cargo fetch --locked --target x86_64-unknown-linux-gnu",
        "cargo audit --json",
        "python3 packaging/release/audit_gate.py --repo . --audit-json",
    ] {
        assert!(
            audit.contains(needle),
            "audit-bridge must run `{needle}`\n{audit}"
        );
    }
}

#[test]
fn publish_is_gated_by_the_release_environment() {
    let body = read(WORKFLOW);
    let publish_section = job(&body, "publish");
    let publish = code_lines(&publish_section);
    assert_eq!(
        job_level(&publish, "environment:"),
        ["release"],
        "publish must declare `environment: release` so the owner can require reviewers"
    );
}

#[test]
fn every_job_has_a_timeout() {
    let body = read(WORKFLOW);
    for name in ["build-bridge", "audit-bridge", "publish"] {
        let section = job(&body, name);
        let lines = code_lines(&section);
        let timeouts = job_level(&lines, "timeout-minutes:");
        assert_eq!(
            timeouts.len(),
            1,
            "`{name}` needs exactly one job-level `timeout-minutes:`: {timeouts:?}"
        );
        let minutes: u32 = timeouts[0].parse().unwrap_or_else(|_| {
            panic!(
                "`{name}` timeout-minutes is not a number: `{}`",
                timeouts[0]
            )
        });
        assert!(
            (1..360).contains(&minutes),
            "`{name}` timeout-minutes must be below GitHub's 360 default, got {minutes}"
        );
    }
}

#[test]
fn no_run_script_interpolates_an_expression() {
    let bodies = run_bodies(&read(WORKFLOW));
    // Guard against a parser that silently finds nothing: it must reach deep
    // into the multi-line publish script.
    assert!(
        bodies
            .iter()
            .any(|b| b.contains("gh release create") && b.contains("--verify-tag")),
        "run-block parser did not find the multi-line `gh release create` script"
    );
    assert!(bodies.len() >= 8, "only found {} run blocks", bodies.len());
    for body in &bodies {
        assert!(
            !body.contains("${{"),
            "a `run:` block interpolates a `${{{{ }}}}` expression (script injection); \
             pass it through `env:` instead:\n{body}"
        );
    }
}

#[test]
fn run_block_parser_handles_inline_block_and_sequence_forms() {
    let sample = "\
steps:
  - run: echo ${{ a }}
  - name: x
    run: |
      line one

      echo ${{ b }}   # still part of the script
    env:
      K: ${{ c }}
  - name: y
    run: >-
      folded ${{ d }}
      more
  # a YAML comment between steps ${{ e }}
  - name: z
    run: echo clean
";
    let bodies = run_bodies(sample);
    assert_eq!(bodies.len(), 4, "{bodies:?}");
    assert!(bodies[0].contains("${{ a }}"));
    assert!(bodies[1].contains("line one") && bodies[1].contains("${{ b }}"));
    assert!(!bodies[1].contains("${{ c }}"), "env: must end the block");
    assert!(bodies[2].contains("${{ d }}") && bodies[2].contains("more"));
    assert!(
        !bodies[2].contains("${{ e }}"),
        "a lower-indented comment ends it"
    );
    assert!(!bodies[3].contains("${{"));
}

#[test]
fn check_install_is_pinned_to_the_sha256_the_build_reported() {
    let body = read(WORKFLOW);
    let code = code_lines(&body).join("\n");
    assert!(
        code.contains("SHA256: ${{ steps.build.outputs.sha256 }}"),
        "the check-install step must take SHA256 from the build step's output"
    );
    let installs: Vec<String> = run_bodies(&body)
        .into_iter()
        .filter(|b| b.contains("check-install.sh"))
        .collect();
    assert_eq!(installs.len(), 1, "expected one check-install.sh run block");
    assert!(
        installs[0].contains("check-install.sh \"$TARBALL\" \"$SHA256\""),
        "check-install.sh must receive the tarball and its expected sha256: {}",
        installs[0]
    );
}

#[test]
fn publish_guards_tag_and_release_state_before_attesting() {
    let body = read(WORKFLOW);
    let publish_section = job(&body, "publish");
    let publish = code_lines(&publish_section);
    let position = |needle: &str| {
        publish
            .iter()
            .position(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("publish job must contain `{needle}`"))
    };
    let attest = position("actions/attest-build-provenance@");
    let tag_check = position("commits/refs/tags/$TAG");
    // Compared with the PEELED commit build-bridge reports, never github.sha
    // (the tag object for an annotated/signed tag push).
    let sha_compare = position("\"$tag_sha\" != \"$BUILT_COMMIT\"");
    position("BUILT_COMMIT: ${{ needs.build-bridge.outputs.commit }}");
    let release_list = position("/releases\" --paginate");
    for (what, at) in [
        ("the tag-commit lookup", tag_check),
        ("the tag-vs-built-commit comparison", sha_compare),
        ("the existing-release check", release_list),
    ] {
        assert!(at < attest, "{what} must run before the attestation step");
    }
}

#[test]
fn release_notes_give_the_strict_verification_command() {
    let body = read(WORKFLOW);
    let code = code_lines(&body).join("\n");
    for needle in [
        "--signer-workflow $GITHUB_REPOSITORY/.github/workflows/release.yml",
        "--source-ref refs/tags/$TAG",
        "--deny-self-hosted-runners",
        "are integrity checks, not authentication",
    ] {
        assert!(
            code.contains(needle),
            "release notes must contain `{needle}`"
        );
    }
    // The pipeline proves a same-job rebuild matches, not full reproducibility.
    assert!(
        !code.contains("reproducibly"),
        "release notes must not claim the build is reproducible"
    );
}

#[test]
fn release_is_created_as_a_draft() {
    let body = read(WORKFLOW);
    let all = code_lines(&body);
    assert_eq!(
        all.iter()
            .filter(|l| l.contains("gh release create"))
            .count(),
        1,
        "release.yml must create the release in exactly one place"
    );
    let publish_section = job(&body, "publish");
    let publish = code_lines(&publish_section);
    let start = publish
        .iter()
        .position(|l| l.contains("gh release create"))
        .expect("`gh release create` must be in the publish job");
    // Join `\`-continued lines into the one shell statement.
    let mut statement = String::new();
    for line in &publish[start..] {
        statement.push_str(line.trim_end_matches('\\'));
        statement.push(' ');
        if !line.ends_with('\\') {
            break;
        }
    }
    for flag in ["--draft", "--verify-tag"] {
        assert!(
            statement.contains(flag),
            "`gh release create` must pass `{flag}`: {statement}"
        );
    }
}

#[test]
fn every_action_is_pinned_to_a_full_commit_sha() {
    let body = read(WORKFLOW);
    let mut checked = 0;
    for line in body.lines().filter(|l| !l.trim_start().starts_with('#')) {
        let Some((_, rest)) = line.split_once("uses:") else {
            continue;
        };
        let (spec, comment) = rest
            .split_once(" #")
            .unwrap_or_else(|| panic!("`{}` needs a trailing `# vX.Y.Z` comment", line.trim()));
        let (_, rev) = spec
            .trim()
            .rsplit_once('@')
            .unwrap_or_else(|| panic!("`{}` has no @<sha> pin", line.trim()));
        assert!(
            is_hex(rev, 40),
            "`{}` must be pinned to a 40-char lowercase hex commit SHA, not `{rev}`",
            line.trim()
        );
        let version = comment.trim();
        assert!(
            version
                .strip_prefix('v')
                .is_some_and(|v| v.starts_with(|c: char| c.is_ascii_digit())),
            "`{}` needs a `# vX.Y.Z` comment, got `# {version}`",
            line.trim()
        );
        checked += 1;
    }
    // checkout, upload-artifact, download-artifact, attest-build-provenance.
    assert!(
        checked >= 4,
        "expected >= 4 pinned `uses:` lines, found {checked}"
    );
}

#[test]
fn workflow_runs_the_release_scripts_that_exist() {
    let body = read(WORKFLOW);
    let code = code_lines(&body).join("\n");
    for script in [
        "install-deps.sh",
        "build-bridge.sh",
        "repro-check.sh",
        "check-install.sh",
    ] {
        let rel = format!("packaging/release/{script}");
        assert!(code.contains(&rel), "release.yml must run `{rel}`");
        assert!(
            workspace_file(&rel).is_file(),
            "release.yml runs missing script `{rel}`"
        );
    }
    assert!(
        code.contains("SW_THROWAWAY_CONTAINER: '1'"),
        "check-install.sh refuses to run without SW_THROWAWAY_CONTAINER=1"
    );
}

#[test]
fn build_script_builds_the_locked_release_bridge_cli() {
    let body = read(BUILD_SCRIPT);
    let command = "cargo build --release --locked -p snitchwatch-bridge-cli --no-default-features";
    assert!(
        code_lines(&body).iter().any(|l| l.contains(command)),
        "{BUILD_SCRIPT} must run `{command}` outside comments"
    );
}

#[test]
fn pins_env_defines_the_release_pins() {
    let pins = pins();
    let get = |key: &str| {
        pins.get(key)
            .map(String::as_str)
            .unwrap_or_else(|| panic!("{PINS} must define {key}"))
    };
    for key in ["SW_RUST_VERSION", "SW_PROTOC_VERSION"] {
        let value = get(key);
        assert!(
            !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit() || b == b'.'),
            "{key} must be a dotted version number, got `{value}`"
        );
    }
    assert_eq!(get("SW_ARCH"), "x86_64", "only x86_64 is built");
    let commit = get("SW_OPENSNITCH_COMMIT");
    assert!(
        is_hex(commit, 40),
        "SW_OPENSNITCH_COMMIT must be a 40-char lowercase hex commit, got `{commit}`"
    );
    assert!(
        !get("SW_OPENSNITCH_TAG").is_empty(),
        "SW_OPENSNITCH_TAG must be set"
    );
}

/// An annotated or signed tag push can hand the workflow the tag OBJECT as
/// `github.sha`; the build must peel it to the commit before build-bridge.sh's
/// `SW_GIT_SHA == HEAD` check, and export that commit for publish's guard.
#[test]
fn build_job_peels_the_pushed_ref_to_a_commit() {
    let body = read(WORKFLOW);
    let build = code_lines(&job(&body, "build-bridge")).join("\n");
    assert!(
        !build.contains("SW_GIT_SHA: ${{ github.sha }}"),
        "SW_GIT_SHA must not be github.sha (a tag object for annotated tags)\n{build}"
    );
    for needle in [
        "rev-parse --verify \"${GITHUB_SHA}^{commit}\"",
        "echo \"SW_GIT_SHA=$sha\" >> \"$GITHUB_ENV\"",
        "commit: ${{ steps.commit.outputs.sha }}",
    ] {
        assert!(
            build.contains(needle),
            "build-bridge must contain `{needle}`\n{build}"
        );
    }
    let resolve = build.find("^{commit}").expect("peel step present");
    let run_build = build
        .find("packaging/release/build-bridge.sh")
        .expect("build step present");
    assert!(
        resolve < run_build,
        "the commit must be resolved before the build runs"
    );
}

/// The MANIFEST records the builder container's full `rpm -qa`, so the CI
/// build must install exactly what the local `just release-*` recipes install
/// (install-deps.sh, no weak deps). A bare `dnf install` here pulled 8 extra
/// weak-dependency packages and made the CI tarball's MANIFEST differ from a
/// local build of the same commit.
#[test]
fn every_dnf_install_skips_weak_deps() {
    let workflow = read(WORKFLOW);
    let deps = read("packaging/release/install-deps.sh");
    for (file, body) in [(WORKFLOW, &workflow), ("install-deps.sh", &deps)] {
        let installs: Vec<&str> = code_lines(body)
            .into_iter()
            .filter(|l| l.contains("dnf ") && l.contains(" install"))
            .collect();
        assert!(
            !installs.is_empty(),
            "{file} should install packages with dnf"
        );
        for line in installs {
            assert!(
                line.contains("--setopt=install_weak_deps=False"),
                "{file}: `{}` must pass --setopt=install_weak_deps=False",
                line.trim()
            );
        }
    }
}

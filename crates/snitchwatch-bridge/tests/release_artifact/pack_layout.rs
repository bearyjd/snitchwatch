use super::*;

#[test]
fn pack_is_byte_identical_and_ignores_stage_modes_and_mtimes() {
    let a = Fixture::standard();
    // A second stage at another path, with a hostile umask and other mtimes.
    let b = Fixture::new(0o700, 0o600, 0o700);
    for rel in [BIN, UNIT, LICENSE] {
        set_mtime(&b.stage.join(rel), 12_345);
    }
    let b_before = snapshot(&b.stage);

    let first = a.pack_ok("out", &[]);
    let again = a.pack_ok("out-again", &[]);
    let other = b.pack_ok("out", &[]);

    let bytes = fs::read(&first).expect("read tarball");
    assert_eq!(
        bytes,
        fs::read(&again).unwrap(),
        "packing twice must be byte-identical"
    );
    assert_eq!(
        bytes,
        fs::read(&other).unwrap(),
        "stage modes/mtimes/path must not leak into the tarball"
    );
    let first_sidecar = fs::read(sidecar(&first)).expect("read .sha256");
    assert_eq!(first_sidecar, fs::read(sidecar(&again)).unwrap());
    assert_eq!(first_sidecar, fs::read(sidecar(&other)).unwrap());
    assert_eq!(
        snapshot(&b.stage),
        b_before,
        "pack must not mutate the stage"
    );
}

#[test]
fn tarball_has_exact_members_modes_owners_and_mtime() {
    let fx = Fixture::standard();
    let tarball = fx.pack_ok("out", &[]);
    let top = top_dir();
    let listing = run_ok(
        Command::new("tar")
            .args(["--numeric-owner", "--full-time", "-tvzf"])
            .arg(&tarball)
            .env("TZ", "UTC")
            .env("LC_ALL", "C"),
    );

    let actual: Vec<(String, String)> = listing
        .lines()
        .map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            assert_eq!(fields.len(), 6, "unexpected `tar -tv` line: {line}");
            assert_eq!(fields[1], "0/0", "uid/gid must be 0/0: {line}");
            assert_eq!(
                format!("{} {}", fields[3], fields[4]),
                SOURCE_DATE_UTC,
                "mtime must be SOURCE_DATE_EPOCH: {line}"
            );
            (fields[0].to_string(), fields[5].to_string())
        })
        .collect();
    let expected: Vec<(String, String)> = [
        ("drwxr-xr-x", ""),
        ("drwxr-xr-x", "usr/"),
        ("drwxr-xr-x", "usr/bin/"),
        ("-rwxr-xr-x", BIN),
        ("drwxr-xr-x", "usr/lib/"),
        ("drwxr-xr-x", "usr/lib/systemd/"),
        ("drwxr-xr-x", "usr/lib/systemd/user/"),
        ("-rw-r--r--", UNIT),
        ("drwxr-xr-x", "usr/share/"),
        ("drwxr-xr-x", "usr/share/licenses/"),
        ("drwxr-xr-x", "usr/share/licenses/snitchwatch-bridge/"),
        ("-rw-r--r--", LICENSE),
        ("drwxr-xr-x", "usr/share/snitchwatch-bridge/"),
        ("-rw-r--r--", MANIFEST),
        ("-rw-r--r--", SUMS),
    ]
    .iter()
    .map(|(mode, rel)| (mode.to_string(), format!("{top}/{rel}")))
    .collect();
    assert_eq!(actual, expected, "tarball members (sorted, dirs included)");

    // The owner *names* are part of the contract too (root/root, not blank).
    let named = run_ok(
        Command::new("tar")
            .arg("-tvzf")
            .arg(&tarball)
            .env("LC_ALL", "C"),
    );
    for line in named.lines() {
        assert_eq!(
            line.split_whitespace().nth(1),
            Some("root/root"),
            "owner: {line}"
        );
    }
}

#[test]
fn manifest_is_canonical_and_its_files_match_the_extracted_bytes() {
    let fx = Fixture::standard();
    let tarball = fx.pack_ok("out", &[]);
    let top = extract(&tarball, &fx.path("x"));
    let (raw, manifest) = read_manifest(&top);

    assert_keys_sorted(&manifest, "$");
    assert_eq!(
        raw,
        serde_json::to_string_pretty(&manifest).unwrap() + "\n",
        "MANIFEST.json must be 2-space indented with sorted keys and a trailing newline"
    );
    assert_eq!(manifest["schema_version"], 1);
    assert_eq!(manifest["name"], "snitchwatch-bridge");
    assert_eq!(manifest["version"], VERSION);
    assert_eq!(manifest["arch"], ARCH);
    assert_eq!(
        manifest["source"],
        serde_json::json!({
            "git_commit": GIT_COMMIT,
            "git_tag": null,
            "dirty": false,
            "source_date_epoch": SOURCE_DATE_EPOCH
        })
    );
    assert_eq!(
        manifest["install"],
        serde_json::json!({
            "binary": "/usr/bin/snitchwatch-bridge-cli",
            "unit": "/usr/lib/systemd/user/snitchwatch-bridge.service",
            "unit_name": "snitchwatch-bridge.service",
            "unit_scope": "user"
        })
    );
    for key in ["build", "upstream", "runtime"] {
        assert_eq!(
            manifest[key],
            meta_json()[key],
            "MANIFEST {key} comes from --meta"
        );
    }

    let files = manifest["files"].as_array().expect("files[] is an array");
    let paths: Vec<&str> = files.iter().map(|f| f["path"].as_str().unwrap()).collect();
    assert_eq!(
        paths,
        [BIN, UNIT, LICENSE],
        "files[]: every file but MANIFEST/SHA256SUMS"
    );
    for entry in files {
        let rel = entry["path"].as_str().unwrap();
        let path = top.join(rel);
        assert_eq!(entry["sha256"], sha256sum(&path), "sha256 of {rel}");
        assert_eq!(
            entry["size"],
            fs::metadata(&path).unwrap().len(),
            "size of {rel}"
        );
        let mode = if rel == BIN { "0755" } else { "0644" };
        assert_eq!(entry["mode"], mode, "mode of {rel}");
    }
}

#[test]
fn sha256sums_passes_sha256sum_check_from_the_top_dir() {
    let fx = Fixture::standard();
    let tarball = fx.pack_ok("out", &[]);
    let top = extract(&tarball, &fx.path("x"));

    let checked = run_ok(
        Command::new("sha256sum")
            .args(["-c", SUMS])
            .current_dir(&top),
    );
    assert_eq!(
        checked.lines().count(),
        4,
        "sha256sum -c output:\n{checked}"
    );
    assert!(checked.lines().all(|l| l.ends_with(": OK")), "{checked}");

    let sums = fs::read_to_string(top.join(SUMS)).unwrap();
    let listed: Vec<&str> = sums
        .lines()
        .map(|line| line.split_once("  ").expect("`<sha256>  <path>` line").1)
        .collect();
    assert_eq!(
        listed,
        [BIN, UNIT, LICENSE, MANIFEST],
        "SHA256SUMS: every file except itself (MANIFEST included), sorted by path"
    );
}

#[test]
fn sha256_sidecar_is_in_sha256sum_format() {
    let fx = Fixture::standard();
    let tarball = fx.pack_ok("out", &[]);
    let text = fs::read_to_string(sidecar(&tarball)).expect("read .sha256");

    assert!(
        text.len() > 64 && text.is_char_boundary(64),
        "sidecar too short: {text:?}"
    );
    let (digest, rest) = text.split_at(64);
    assert!(
        digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "digest must be 64 lowercase hex: {text:?}"
    );
    assert_eq!(rest, format!("  {}\n", tarball_name()), "sidecar: {text:?}");
    assert_eq!(
        digest,
        sha256sum(&tarball),
        "sidecar digest must match the tarball"
    );
    let out_dir = tarball.parent().unwrap();
    run_ok(
        Command::new("sha256sum")
            .arg("-c")
            .arg(format!("{}.sha256", tarball_name()))
            .current_dir(out_dir),
    );
}

#[test]
fn verify_accepts_a_freshly_packed_tarball() {
    let fx = Fixture::standard();
    let tarball = fx.pack_ok("out", &[]);
    let res = verify(&tarball);
    assert!(res.status.success(), "{}", describe("verify", &res));
    assert!(String::from_utf8_lossy(&res.stdout).contains("verify: OK"));
}

/// Extract `tarball`, optionally flip one byte of the binary (same length, so
/// only hashes can notice), repack with the system tar honouring every other
/// part of the contract, and write a *matching* outer `.sha256`.
fn repack(fx: &Fixture, tarball: &Path, label: &str, tamper: bool) -> PathBuf {
    let extracted = fx.path(&format!("{label}-x"));
    let top = extract(tarball, &extracted);
    if tamper {
        let bin = top.join(BIN);
        let mut bytes = fs::read(&bin).expect("read extracted binary");
        let middle = bytes.len() / 2;
        bytes[middle] ^= 0x01;
        fs::write(&bin, bytes).expect("write tampered binary");
    }
    let out_dir = fx.path(&format!("{label}-out"));
    fs::create_dir_all(&out_dir).expect("create repack out dir");
    let repacked = out_dir.join(tarball_name());
    run_ok(
        Command::new("tar")
            .args([
                "--format=ustar",
                "--sort=name",
                "--owner=root:0",
                "--group=root:0",
            ])
            .arg(format!("--mtime=@{SOURCE_DATE_EPOCH}"))
            .arg("--mode=a+rX,u+w,go-w")
            .arg("-czf")
            .arg(&repacked)
            .arg("-C")
            .arg(&extracted)
            .arg(top_dir()),
    );
    let digest_line = run_ok(
        Command::new("sha256sum")
            .arg(tarball_name())
            .current_dir(&out_dir),
    );
    fs::write(sidecar(&repacked), digest_line).expect("write regenerated .sha256");
    repacked
}

#[test]
fn verify_rejects_a_repacked_tarball_with_a_modified_binary() {
    let fx = Fixture::standard();
    let tarball = fx.pack_ok("out", &[]);

    // Control: the same repack *without* tampering passes, so the failure
    // below is caused by the binary bytes, not by how we repacked. GNU gzip's
    // bytes differ from Python's, yet verify accepts it: it compares the
    // uncompressed tar stream (byte-identical with GNU tar's USTAR output),
    // never the zlib-implementation-dependent gzip bytes.
    let control = repack(&fx, &tarball, "control", false);
    assert_ne!(
        fs::read(&control).unwrap(),
        fs::read(&tarball).unwrap(),
        "the control must exercise a different gzip encoding"
    );
    let res = verify(&control);
    assert!(
        res.status.success(),
        "{}",
        describe("verify (control repack)", &res)
    );

    let tampered = repack(&fx, &tarball, "tampered", true);
    let res = verify(&tampered);
    assert!(
        !res.status.success(),
        "verify must reject a modified binary:\n{}",
        describe("verify", &res)
    );
    let stderr = String::from_utf8_lossy(&res.stderr);
    let fails: Vec<&str> = stderr
        .lines()
        .filter(|l| l.starts_with("verify: FAIL: "))
        .collect();
    assert!(
        fails
            .iter()
            .any(|l| l.contains("MANIFEST") && l.contains(BIN) && l.contains("sha256")),
        "MANIFEST files[] must catch the modified binary:\n{stderr}"
    );
    assert!(
        fails
            .iter()
            .any(|l| l.contains("SHA256SUMS") && l.contains(BIN)),
        "SHA256SUMS must catch the modified binary:\n{stderr}"
    );
    assert!(
        !stderr.contains(".tar.gz.sha256 is"),
        "the regenerated outer .sha256 matches, so it must not be the failure:\n{stderr}"
    );
}

/// Plan decision I: the release binary is built with `--no-default-features`,
/// so it must not embed the GPL-2.0-only `web/` frontend. A binary carrying the
/// copyright line every `web/` file has is rejected by plain `verify`.
#[test]
fn verify_rejects_a_binary_that_embeds_the_web_ui() {
    let fx = Fixture::standard();
    let mut bytes = FAKE_BINARY.to_vec();
    bytes.extend_from_slice(b"/* Copyright (C) 2026 Objective Development Software GmbH */");
    write_file(&fx.stage.join(BIN), &bytes, 0o755);
    let tarball = fx.pack_ok("out-webui", &[]);
    assert_rejected(&tarball, "embeds the vendored web/ UI", "web UI embedded");
}

#[test]
fn pack_refuses_a_git_tag_that_does_not_match_the_version() {
    let fx = Fixture::standard();
    let out = fx.path("out");
    let res = fx.pack(&out, &["--git-tag", "v9.9.9"]);
    assert!(!res.status.success(), "{}", describe("pack", &res));
    assert!(
        String::from_utf8_lossy(&res.stderr).contains("--git-tag"),
        "{}",
        describe("pack", &res)
    );
    assert!(!out.exists(), "a refused pack must not write anything");

    // Control: the matching tag is accepted and recorded.
    let tag = format!("v{VERSION}");
    let tarball = fx.pack_ok("out-tagged", &["--git-tag", &tag]);
    let (_, manifest) = read_manifest(&extract(&tarball, &fx.path("x")));
    assert_eq!(manifest["source"]["git_tag"], tag);
}

#[test]
fn pack_refuses_an_unexpected_file_in_the_stage() {
    let fx = Fixture::standard();
    write_file(&fx.stage.join("usr/bin/extra-tool"), b"#!/bin/sh\n", 0o755);
    let out = fx.path("out");
    let res = fx.pack(&out, &[]);
    assert!(!res.status.success(), "{}", describe("pack", &res));
    assert!(
        String::from_utf8_lossy(&res.stderr).contains("usr/bin/extra-tool"),
        "{}",
        describe("pack", &res)
    );
    assert!(!out.exists(), "a refused pack must not write anything");
}

#[test]
fn pack_refuses_a_symlink_in_the_stage() {
    let fx = Fixture::standard();
    std::os::unix::fs::symlink(
        "/etc/passwd",
        fx.stage
            .join("usr/share/licenses/snitchwatch-bridge/LICENSE.link"),
    )
    .expect("create symlink");
    let out = fx.path("out");
    let res = fx.pack(&out, &[]);
    assert!(!res.status.success(), "{}", describe("pack", &res));
    assert!(
        String::from_utf8_lossy(&res.stderr).contains("LICENSE.link"),
        "{}",
        describe("pack", &res)
    );
    assert!(!out.exists(), "a refused pack must not write anything");
}

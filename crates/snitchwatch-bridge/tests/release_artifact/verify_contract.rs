use super::*;

#[test]
fn verify_rejects_malformed_manifest_values() {
    let fx = Fixture::standard();
    let tarball = fx.pack_ok("out", &[]);
    let tag = format!("v{VERSION}");
    let cases: Vec<(String, &str)> = vec![
        (r#"m["build"] = {}"#.into(), "MANIFEST build keys []"),
        (r#"m["upstream"] = {}"#.into(), "MANIFEST upstream keys []"),
        (
            r#"m["runtime"]["glibc_min"] = "banana""#.into(),
            "glibc_min 'banana'",
        ),
        (
            r#"m["files"][0]["size"] = float(m["files"][0]["size"])"#.into(),
            "is not an integer",
        ),
        (
            format!(r#"m["source"].update(dirty=True, git_tag="{tag}")"#),
            "on a dirty tree",
        ),
        (
            r#"m["runtime"]["needed"] = ["libssl.so.3"]"#.into(),
            "libssl.so.3",
        ),
        (
            r#"m["build"]["rpm_qa_sha256"] = "0" * 64"#.into(),
            "rpm_qa_sha256",
        ),
        (
            r#"m["build"]["rpm_qa"].reverse()"#.into(),
            "rpm_qa is not sorted",
        ),
        (
            r#"m["build"]["rpms"]["rust"] = "rust-9-9""#.into(),
            "not in build.rpm_qa",
        ),
        (
            r#"m["build"]["cargo_lock_sha256"] = "XYZ""#.into(),
            "cargo_lock_sha256",
        ),
        (
            r#"m["build"]["builder_image"] = "fedora:44""#.into(),
            "not digest-pinned",
        ),
        (
            r#"m["upstream"]["opensnitch_commit"] = "b404""#.into(),
            "opensnitch_commit",
        ),
        (r#"m["source"]["git_commit"] += "\n""#.into(), "git_commit"),
        (
            r#"m["source"]["dirty"] = 0"#.into(),
            "dirty 0 is not a boolean",
        ),
        (
            r#"m["files"][0]["path"] = ["x"]"#.into(),
            "path ['x'] is not a string",
        ),
        (r#"m["files"][0]["extra"] = 1"#.into(), "files[] entry keys"),
        (
            r#"m["schema_version"] = True"#.into(),
            "schema_version is True",
        ),
    ];
    for (i, (statement, needle)) in cases.iter().enumerate() {
        let bad = mutate(&fx, &tarball, &format!("m{i}"), "manifest", statement);
        assert_rejected(&bad, needle, statement);
    }
}

#[test]
fn verify_never_tracebacks_on_wrongly_typed_manifest_sections() {
    let fx = Fixture::standard();
    let tarball = fx.pack_ok("out", &[]);
    let sections = ["source", "build", "upstream", "runtime", "install", "files"];
    let mut i = 0;
    for section in sections {
        for value in ["[]", "'x'", "1", "None", "[1]", "{'a': []}"] {
            let statement = format!("m[{section:?}] = {value}");
            let bad = mutate(&fx, &tarball, &format!("t{i}"), "manifest", &statement);
            assert_rejected(&bad, "MANIFEST", &statement);
            i += 1;
        }
    }
    let statement = "m['build']['rpms'] = []; m['runtime']['needed'] = 'libc.so.6'";
    let bad = mutate(&fx, &tarball, "nested", "manifest", statement);
    assert_rejected(&bad, "build.rpms", statement);
}

#[test]
fn pack_refuses_git_tag_together_with_dirty() {
    let fx = Fixture::standard();
    let out = fx.path("out");
    let tag = format!("v{VERSION}");
    let res = fx.pack(&out, &["--git-tag", &tag, "--dirty"]);
    assert!(!res.status.success(), "{}", describe("pack", &res));
    assert!(
        String::from_utf8_lossy(&res.stderr).contains("--git-tag and --dirty"),
        "{}",
        describe("pack", &res)
    );
    assert!(!out.exists(), "a refused pack must not write anything");

    // Control: --dirty alone is accepted and recorded.
    let tarball = fx.pack_ok("out-dirty", &["--dirty"]);
    let (_, manifest) = read_manifest(&extract(&tarball, &fx.path("x")));
    assert_eq!(manifest["source"]["dirty"], true);
}

#[test]
fn verify_refuses_to_run_or_ldd_the_binary_without_expect_sha256() {
    let fx = Fixture::standard();
    let tarball = fx.pack_ok("out", &[]);
    for flag in ["--run-flags", "--check-ldd"] {
        let res = verify_with(&tarball, &[flag]);
        assert!(
            !res.status.success(),
            "{flag}: {}",
            describe("verify", &res)
        );
        assert!(
            fail_lines(&res)
                .iter()
                .any(|l| l.contains("pass --expect-sha256")),
            "{flag}: {}",
            describe("verify", &res)
        );
    }
}

#[test]
fn verify_checks_expect_sha256_before_anything_else() {
    let fx = Fixture::standard();
    let tarball = fx.pack_ok("out", &[]);
    let digest = sha256sum(&tarball);
    let res = verify_with(&tarball, &["--expect-sha256", &digest]);
    assert!(
        res.status.success(),
        "{}",
        describe("verify (control)", &res)
    );

    // With the sidecar gone too, the mismatch must be the ONLY failure: the
    // untrusted bytes are not even parsed.
    fs::remove_file(sidecar(&tarball)).unwrap();
    let wrong = format!(
        "{}{}",
        &digest[..63],
        if digest.ends_with('0') { "1" } else { "0" }
    );
    let res = verify_with(&tarball, &["--expect-sha256", &wrong]);
    assert!(!res.status.success(), "{}", describe("verify", &res));
    let fails = fail_lines(&res);
    assert_eq!(fails.len(), 1, "{fails:?}");
    assert!(fails[0].contains("--expect-sha256"), "{fails:?}");

    let res = verify_with(&tarball, &["--expect-sha256", &digest.to_uppercase()]);
    assert!(!res.status.success(), "{}", describe("verify", &res));
    assert!(fail_lines(&res)[0].contains("64 lowercase hex"));
}

#[test]
fn verify_extract_to_writes_exactly_the_verified_tree_with_contract_modes() {
    let fx = Fixture::standard();
    let tarball = fx.pack_ok("out", &[]);
    let digest = sha256sum(&tarball);
    let dest = fx.path("installed");
    let dest_arg = dest.to_str().unwrap();
    let res = verify_with(
        &tarball,
        &["--expect-sha256", &digest, "--extract-to", dest_arg],
    );
    assert!(res.status.success(), "{}", describe("verify", &res));
    let entries: Vec<String> = fs::read_dir(&dest)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        entries,
        [top_dir()],
        "only the top dir, no staging leftovers"
    );

    let reference = snapshot(&extract(&tarball, &fx.path("reference")));
    let got = snapshot(&dest.join(top_dir()));
    let names = |s: &[(PathBuf, u32, SystemTime, Vec<u8>)]| -> Vec<PathBuf> {
        s.iter().map(|e| e.0.clone()).collect()
    };
    assert_eq!(names(&got), names(&reference), "same paths as the tarball");
    let epoch = SystemTime::UNIX_EPOCH + Duration::from_secs(SOURCE_DATE_EPOCH);
    for ((rel, mode, mtime, bytes), (_, _, _, want_bytes)) in got.iter().zip(&reference) {
        let is_file = top_dir_relative_is_file(rel);
        let want_mode = if !is_file || rel.ends_with(BIN) {
            0o755
        } else {
            0o644
        };
        assert_eq!(mode & 0o7777, want_mode, "mode of {}", rel.display());
        assert_eq!(*mtime, epoch, "mtime of {}", rel.display());
        assert_eq!(bytes, want_bytes, "bytes of {}", rel.display());
    }
    assert_eq!(
        fs::metadata(dest.join(top_dir()))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777,
        0o755
    );

    // Never merges into an existing tree.
    let res = verify_with(
        &tarball,
        &["--expect-sha256", &digest, "--extract-to", dest_arg],
    );
    assert!(
        !res.status.success(),
        "{}",
        describe("verify (again)", &res)
    );
    assert!(String::from_utf8_lossy(&res.stderr).contains("already exists"));

    // A tarball that fails verification writes nothing.
    let bad = mutate(&fx, &tarball, "tamper", "tamper", "");
    let bad_digest = sha256sum(&bad);
    let nowhere = fx.path("never");
    let res = verify_with(
        &bad,
        &[
            "--expect-sha256",
            &bad_digest,
            "--extract-to",
            nowhere.to_str().unwrap(),
        ],
    );
    assert!(
        !res.status.success(),
        "{}",
        describe("verify (tampered)", &res)
    );
    assert!(
        !nowhere.exists(),
        "a failed verify must not extract anything"
    );
}

/// `snapshot` paths are relative to the top dir; files are the contract files.
fn top_dir_relative_is_file(rel: &Path) -> bool {
    let rel = rel.to_string_lossy();
    rel == BIN || rel == UNIT || rel == LICENSE || rel == MANIFEST || rel == SUMS
}

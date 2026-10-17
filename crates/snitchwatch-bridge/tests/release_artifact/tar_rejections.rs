use super::*;

#[test]
fn mutate_helper_identity_still_verifies() {
    let fx = Fixture::standard();
    let tarball = fx.pack_ok("out", &[]);
    let same = mutate(&fx, &tarball, "identity", "identity", "");
    assert_eq!(fs::read(&same).unwrap(), fs::read(&tarball).unwrap());
    let res = verify(&same);
    assert!(res.status.success(), "{}", describe("verify", &res));
}

#[test]
fn verify_rejects_a_setuid_binary() {
    let case = "setuid";
    rejects_header_change(case, BIN, "mode", 0o4755, &format!("{BIN}: mode 4755"));
}

#[test]
fn verify_rejects_a_group_writable_directory() {
    rejects_header_change("dir0775", "usr/bin", "mode", 0o775, "usr/bin: mode 0775");
}

#[test]
fn verify_rejects_a_member_owned_by_uid_1000() {
    rejects_header_change("uid1000", UNIT, "uid", 1000, "is owned by (1000, 0,");
}

#[test]
fn verify_rejects_an_mtime_off_by_one() {
    let mtime = SOURCE_DATE_EPOCH + 1;
    rejects_header_change("mtime", BIN, "mtime", mtime, "member mtimes");
}

#[test]
fn verify_rejects_a_hardlink_member() {
    let fx = Fixture::standard();
    let tarball = fx.pack_ok("out", &[]);
    let bad = mutate(&fx, &tarball, "hardlink", "hardlink", "");
    assert_rejected(&bad, "is not a regular file or directory", "hardlink");
}

#[test]
fn verify_rejects_a_dotdot_member() {
    let fx = Fixture::standard();
    let tarball = fx.pack_ok("out", &[]);
    let bad = mutate(&fx, &tarball, "dotdot", "dotdot", "");
    assert_rejected(&bad, "unsafe member path '../evil'", "../evil");
}

#[test]
fn verify_rejects_a_tarball_without_license_files() {
    let fx = Fixture::standard();
    let tarball = fx.pack_ok("out", &[]);
    let bad = mutate(&fx, &tarball, "nolicense", "drop-license", "");
    assert_rejected(&bad, "no license files under", "no license");
}

#[test]
fn verify_rejects_a_bad_sha256_sidecar() {
    let fx = Fixture::standard();
    let tarball = fx.pack_ok("out", &[]);
    let path = sidecar(&tarball);
    let good = fs::read_to_string(&path).unwrap();
    let wrong_digest = format!("{}{}", "0".repeat(64), &good[64..]);
    let one_space = good.replacen("  ", " ", 1);
    for (case, text) in [
        ("wrong digest", Some(wrong_digest)),
        ("format", Some(one_space)),
        ("missing", None),
    ] {
        match &text {
            Some(text) => fs::write(&path, text).unwrap(),
            None => fs::remove_file(&path).unwrap(),
        }
        let res = verify(&tarball);
        assert!(
            !res.status.success(),
            "{case}: {}",
            describe("verify", &res)
        );
        let fails = fail_lines(&res);
        assert!(
            fails.iter().any(|l| l.contains(".tar.gz.sha256")),
            "{case}: the sidecar must be the failure:\n{}",
            describe("verify", &res)
        );
    }
}

#[test]
fn verify_rejects_a_member_appended_after_the_end_of_archive() {
    let fx = Fixture::standard();
    let tarball = fx.pack_ok("out", &[]);
    let bad = mutate(&fx, &tarball, "trailing", "append-after-eof", "");
    // The differential: GNU tar --ignore-zeros (and other lenient readers)
    // still see the smuggled member that Python's tarfile stops before.
    let listing = run(Command::new("tar")
        .args(["--ignore-zeros", "-tzf"])
        .arg(&bad));
    assert!(String::from_utf8_lossy(&listing.stdout).contains("usr/bin/evil"));
    let fails = assert_rejected(&bad, "bytes after the canonical end-of-archive", "trailing");
    assert_eq!(
        fails.len(),
        1,
        "only the canonical-stream check can see it: {fails:?}"
    );
}

#[test]
fn verify_rejects_a_member_after_a_corrupt_header() {
    let fx = Fixture::standard();
    let tarball = fx.pack_ok("out", &[]);
    let bad = mutate(&fx, &tarball, "corrupt", "after-corrupt-header", "");
    // GNU tar skips the bad block ("Skipping to next header") and lists the
    // member behind it; Python's tarfile silently stops at the bad block.
    let listing = run(Command::new("tar").arg("-tzf").arg(&bad));
    assert!(String::from_utf8_lossy(&listing.stdout).contains("usr/bin/evil"));
    let fails = assert_rejected(&bad, "not the canonical serialization", "corrupt header");
    assert_eq!(
        fails.len(),
        1,
        "only the canonical-stream check can see it: {fails:?}"
    );
}

#[test]
fn verify_rejects_a_pax_xattr_header() {
    let fx = Fixture::standard();
    let tarball = fx.pack_ok("out", &[]);
    let bad = mutate(&fx, &tarball, "pax", "pax-xattr", "");
    assert_rejected(&bad, "SCHILY.xattr.security.capability", "pax xattr");
}

#[test]
fn verify_rejects_a_second_gzip_member() {
    let fx = Fixture::standard();
    let tarball = fx.pack_ok("out", &[]);
    let bad = mutate(&fx, &tarball, "gzip2", "gzip-second-member", "");
    assert_rejected(
        &bad,
        "trailing data after the gzip member",
        "second gzip member",
    );
}

#[test]
fn verify_rejects_a_modified_binary_in_a_canonical_tarball() {
    let fx = Fixture::standard();
    let tarball = fx.pack_ok("out", &[]);
    let bad = mutate(&fx, &tarball, "tamper", "tamper", "");
    let fails = assert_rejected(&bad, "SHA256SUMS does not match", "tampered binary");
    assert!(
        fails.iter().all(|l| !l.contains("canonical")),
        "the tar stream itself is canonical here: {fails:?}"
    );
}

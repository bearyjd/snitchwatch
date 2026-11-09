//! Shape + behaviour tests for `packaging/ebpf/build-modules.sh` and
//! `packaging/ebpf/pins.env` (PR-E1: build the opensnitch eBPF modules from
//! the pinned vendor commit). No clang, kernel headers, sudo, podman or
//! daemon is needed: only `--help`, `--dry-run` and the argument/pin/source
//! failure paths are executed. The real build is exercised on the tower.
//!
//! The script is executed directly (never `bash <script>`): only direct
//! execution honours its `#!/bin/bash -p` shebang and environment re-exec.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const SCRIPT: &str = "packaging/ebpf/build-modules.sh";
const PINS: &str = "packaging/ebpf/pins.env";
const RELEASE_PINS: &str = "packaging/release/pins.env";
/// A kernel tree path that satisfies the lexical rules for the pinned
/// KERNEL_VER (7.2.15); dry-run does not require it to exist.
const DRY_KDIR: &str = "/usr/src/kernels/7.2.15-200.fc45.x86_64";

fn workspace_file(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel)
}

fn read(rel: &str) -> String {
    let path = workspace_file(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {}", path.display(), e))
}

fn parse_env(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_string(), v.trim_matches('"').to_string()))
        .collect()
}

fn vendor() -> String {
    workspace_file("vendor/opensnitch")
        .canonicalize()
        .unwrap()
        .to_str()
        .unwrap()
        .to_string()
}

fn run_script(script: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(script);
    cmd.args(args);
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.output().expect("spawn script")
}

fn run(args: &[&str], env: &[(&str, &str)]) -> Output {
    run_script(&workspace_file(SCRIPT), args, env)
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

fn fails_with(o: &Output, needle: &str) {
    assert!(!o.status.success(), "expected failure: {}", text(o));
    assert!(text(o).contains(needle), "lacks {needle:?}: {}", text(o));
}

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("sw-ebpf-shape-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Valid dry-run arguments with a fresh, empty staging dir.
fn dry_args<'a>(stage: &'a str, os_dir: &'a str) -> Vec<&'a str> {
    vec![
        "--dry-run",
        "--staging",
        stage,
        "--kernel-dir",
        DRY_KDIR,
        "--opensnitch-dir",
        os_dir,
    ]
}

fn set_pin(pins: &str, key: &str, value: &str) -> String {
    pins.lines()
        .map(|l| {
            if l.starts_with(&format!("{key}=")) {
                format!("{key}={value}")
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Copy of the script into `<d>/packaging/{ebpf,release}/` with edited pins
/// (the script reads its pins only from next to itself).
fn layout(d: &Path, ebpf: &[(&str, &str)], release: &[(&str, &str)]) -> PathBuf {
    let e = d.join("packaging/ebpf");
    let r = d.join("packaging/release");
    std::fs::create_dir_all(&e).unwrap();
    std::fs::create_dir_all(&r).unwrap();
    std::fs::copy(workspace_file(SCRIPT), e.join("build-modules.sh")).unwrap();
    let edit =
        |text: String, kv: &[(&str, &str)]| kv.iter().fold(text, |t, (k, v)| set_pin(&t, k, v));
    std::fs::write(e.join("pins.env"), edit(read(PINS), ebpf)).unwrap();
    std::fs::write(r.join("pins.env"), edit(read(RELEASE_PINS), release)).unwrap();
    e.join("build-modules.sh")
}

fn git(dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(o.status.success(), "git {args:?}: {}", text(&o));
    String::from_utf8(o.stdout).unwrap().trim().to_string()
}

/// A scratch git repo whose single commit holds `files` under `ebpf_prog/`
/// (plus `extra` at the repo root); returns (repo, commit).
fn repo(d: &Path, files: &[(&str, &str)], extra: &[(&str, &str)]) -> (PathBuf, String) {
    let r = d.join("repo");
    std::fs::create_dir_all(r.join("ebpf_prog")).unwrap();
    for (p, c) in files {
        let f = r.join("ebpf_prog").join(p);
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(f, c).unwrap();
    }
    for (p, c) in extra {
        std::fs::write(r.join(p), c).unwrap();
    }
    git(&r, &["init", "-q"]);
    git(&r, &["add", "-A"]);
    git(&r, &["commit", "-q", "-m", "t"]);
    let head = git(&r, &["rev-parse", "HEAD"]);
    (r, head)
}

/// The documented tree hash recipe, computed independently of git.
fn tree_hash(dir: &Path) -> String {
    let o = Command::new("/bin/bash")
        .arg("-c")
        .arg("cd \"$1\" && find . -type f -print0 | sort -z | xargs -0 sha256sum | sha256sum")
        .arg("_")
        .arg(dir)
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    assert!(o.status.success());
    String::from_utf8(o.stdout).unwrap()[..64].to_string()
}

const SRC: &[(&str, &str)] = &[
    ("Makefile", "all:\n\t@echo $Format:%H$\n"),
    ("bpf_headers/a.h", "/* a */\n"),
];

#[test]
fn opensnitch_commit_pin_equals_release_pin() {
    let ebpf = parse_env(&read(PINS));
    let release = parse_env(&read(RELEASE_PINS));
    let pin = ebpf
        .get("SW_OPENSNITCH_COMMIT")
        .expect("SW_OPENSNITCH_COMMIT");
    assert_eq!(pin.len(), 40);
    assert!(pin
        .bytes()
        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
    assert_eq!(Some(pin), release.get("SW_OPENSNITCH_COMMIT"));
}

#[test]
fn toolchain_and_tree_pins_present_and_well_formed() {
    let ebpf = parse_env(&read(PINS));
    for k in [
        "SW_EBPF_CLANG_MAJOR",
        "SW_EBPF_LLVM_MAJOR",
        "SW_EBPF_LLC_MAJOR",
    ] {
        let v = ebpf.get(k).unwrap_or_else(|| panic!("{k} missing"));
        assert!(
            !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()),
            "{k}={v}"
        );
    }
    assert!(ebpf
        .get("SW_EBPF_KERNEL_DEVEL_VERSION")
        .is_some_and(|v| !v.is_empty()));
    let t = ebpf.get("SW_EBPF_PROG_TREE_SHA256").expect("tree pin");
    assert_eq!(t.len(), 64);
    assert!(t
        .bytes()
        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
}

#[test]
fn script_shape() {
    let s = read(SCRIPT);
    assert!(s.starts_with("#!/bin/bash -p\n"));
    for needle in [
        "[[ -n ${SW_EBPF_CLEAN:-} ]] || exec /usr/bin/env -i SW_EBPF_CLEAN=1 PATH=/usr/bin:/bin LC_ALL=C /bin/bash -p -- \"$0\" \"$@\"",
        "compgen -e",
        "set -euo pipefail -o noclobber",
        "--help",
        "--dry-run",
        "--kernel-arch",
        "llvm-strip -g",
        "SHA256SUMS",
        "usr/lib/opensnitchd/ebpf",
        "opensnitch.o",
        "opensnitch-procs.o",
        "opensnitch-dns.o",
        "mktemp -d",
        "install -m 0644 --",
        "SW_OPENSNITCH_COMMIT",
        "SW_EBPF_PROG_TREE_SHA256",
        "ls-tree -r -z --full-tree",
        "cat-file blob",
        "--no-replace-objects",
        "realpath -e --",
        "realpath -m --",
        "/usr/src/kernels/",
        "kernel.release",
        "rpm -qf",
        "CC=\"$CLANG_BIN -DCC_USING_FENTRY\"",
        "--allow-kernel-mismatch",
        "LLC=\"$LLC_BIN\"",
        "-i PATH=\"$SAFE_PATH\" HOME=/nonexistent LC_ALL=C",
        "GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null",
        "clean sha256sum",
        "clean install",
        "clean mktemp",
        "clean realpath",
        "ENV_BIN=/usr/bin/env",
        "only x86_64 is supported",
        "clean \"$STRIP_BIN\" -g",
        "\"$MAKE_BIN\" -B -R -r -C",
        "KERNEL_ARCH=\"$KARCH\"",
        "invalid KERNEL_ARCH",
        "\"$LLC_BIN\" --version",
        "release pins not found",
    ] {
        assert!(s.contains(needle), "script lacks {needle:?}");
    }
    for banned in [
        "sudo",
        "podman",
        "cp -R",
        "env -u",
        "#!/usr/bin/env",
        "SW_EBPF_PINS_FILE",
        "${KERNEL_DIR:-}",
        "${KERNEL_HEADERS:-}",
        "${KERNEL_ARCH:-",
        " archive ",
        "make -C \"$WORK\" clean",
        "\n. \"$PINS_FILE\"",
        "make -C vendor",
        "git -C vendor/opensnitch checkout",
    ] {
        assert!(!s.contains(banned), "script must not contain {banned:?}");
    }
}

#[test]
fn help_exits_zero() {
    let o = run(&["--help"], &[]);
    assert!(o.status.success(), "{}", text(&o));
    let t = text(&o);
    assert!(t.contains("--dry-run") && t.contains("--staging"), "{t}");
}

#[test]
fn dry_run_verifies_the_pinned_tree_and_writes_nothing() {
    let stage = scratch("dry");
    let os = vendor();
    let o = run(&dry_args(stage.to_str().unwrap(), &os), &[]);
    let t = text(&o);
    assert!(o.status.success(), "{t}");
    assert!(t.contains("dry-run"), "{t}");
    assert!(t.contains("tree hash ok"), "{t}");
    assert!(t.contains("usr/lib/opensnitchd/ebpf"), "{t}");
    assert_eq!(
        std::fs::read_dir(&stage).unwrap().count(),
        0,
        "dry-run wrote into staging"
    );
}

#[test]
fn hostile_environment_runs_nothing() {
    let d = scratch("env");
    let marker = d.join("pwned");
    let m = marker.to_str().unwrap();
    let bin = d.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    for tool in ["env", "bash", "sed", "git", "realpath"] {
        let f = bin.join(tool);
        std::fs::write(&f, format!("#!/bin/sh\ntouch {m}\nexit 1\n")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let rc = d.join("rc");
    std::fs::write(&rc, format!("touch {m}\n")).unwrap();
    let func = format!("() {{ /usr/bin/touch {m}; }}");
    let path = format!("{}:/usr/bin:/bin", bin.display());
    let ps4 = format!("$(/usr/bin/touch {m})");
    let os = vendor();
    let cases: Vec<Vec<(&str, &str)>> = vec![
        vec![("BASH_ENV", rc.to_str().unwrap())],
        vec![("ENV", rc.to_str().unwrap())],
        vec![("BASH_FUNC_exec%%", &func)],
        vec![("BASH_FUNC_set%%", &func)],
        vec![("BASH_FUNC_echo%%", &func)],
        vec![("SHELLOPTS", "xtrace"), ("PS4", &ps4)],
        vec![("PATH", &path)],
        vec![("CDPATH", d.to_str().unwrap())],
        vec![("SW_EBPF_PINS_FILE", "/nonexistent")],
    ];
    for env in cases {
        let stage = d.join("stage");
        let _ = std::fs::remove_dir_all(&stage);
        let o = run(&dry_args(stage.to_str().unwrap(), &os), &env);
        assert!(o.status.success(), "{env:?}: {}", text(&o));
        assert!(!marker.exists(), "{env:?} ran code");
    }
}

#[test]
fn forged_clean_marker_is_rejected() {
    let d = scratch("forged");
    let os = vendor();
    let o = run(
        &dry_args(d.to_str().unwrap(), &os),
        &[
            ("SW_EBPF_CLEAN", "1"),
            ("PATH", "/usr/bin:/bin"),
            ("LC_ALL", "C"),
            ("KERNEL_ARCH", "aarch64"),
        ],
    );
    fails_with(&o, "unexpected environment");
}

#[test]
fn kernel_inputs_come_from_flags_only() {
    let d = scratch("envk");
    let st = d.to_str().unwrap();
    let o = run(
        &["--staging", st],
        &[("KERNEL_DIR", DRY_KDIR), ("KERNEL_HEADERS", DRY_KDIR)],
    );
    fails_with(&o, "--kernel-dir is required");
    assert!(!text(&o).contains("env KERNEL_DIR"), "{}", text(&o));
    // An inherited KERNEL_ARCH is ignored (the re-exec drops it).
    let os = vendor();
    let o = run(&dry_args(st, &os), &[("KERNEL_ARCH", "aarch64")]);
    assert!(o.status.success(), "{}", text(&o));
}

#[test]
fn unknown_flag_fails() {
    let o = run(&["--bogus"], &[]);
    fails_with(&o, "unknown");
}

fn with<'a>(mut base: Vec<&'a str>, extra: &[&'a str]) -> Vec<&'a str> {
    base.extend_from_slice(extra);
    base
}

#[test]
fn make_expression_in_kernel_ver_is_rejected() {
    let d = scratch("kver");
    let os = vendor();
    let st = d.to_str().unwrap();
    for bad in [
        "$(shell touch pwned)",
        "6.1;x",
        "6 1",
        "`id`",
        "7",
        "x.1",
        "7.x",
        ".7.2",
    ] {
        let o = run(&with(dry_args(st, &os), &["--kernel-ver", bad]), &[]);
        fails_with(&o, "invalid KERNEL_VER");
    }
    assert!(!d.join("pwned").exists());
}

#[test]
fn kernel_tree_must_be_the_pinned_kernel_under_usr_src_kernels() {
    let d = scratch("kpath");
    let os = vendor();
    let st = d.join("s");
    let st = st.to_str().unwrap();
    let evil = d.join("a$(shell touch pwned)");
    std::fs::create_dir_all(&evil).unwrap();
    let cases: &[(&str, &str)] = &[
        (evil.to_str().unwrap(), "invalid KERNEL_DIR"),
        (d.to_str().unwrap(), "KERNEL_DIR must be /usr/src/kernels/"),
        ("rel/dir", "KERNEL_DIR must be /usr/src/kernels/"),
        ("/usr/src/kernels", "KERNEL_DIR must be /usr/src/kernels/"),
        (
            "/usr/src/kernels/7.2.15-x/include",
            "KERNEL_DIR must be /usr/src/kernels/",
        ),
        (
            "/usr/src/kernels/7.1.13-100.fc43.x86_64",
            "does not match KERNEL_VER",
        ),
        (
            "/usr/src/kernels/7.2.150-1.fc45.x86_64",
            "does not match KERNEL_VER",
        ),
    ];
    for (kdir, msg) in cases {
        let args = vec![
            "--dry-run",
            "--staging",
            st,
            "--kernel-dir",
            kdir,
            "--opensnitch-dir",
            &os,
        ];
        fails_with(&run(&args, &[]), msg);
    }
    let o = run(&with(dry_args(st, &os), &["--kernel-headers", "/tmp"]), &[]);
    fails_with(&o, "KERNEL_HEADERS must be /usr/src/kernels/");
    assert!(!d.join("pwned").exists());
}

#[test]
fn real_run_needs_existing_kernel_tree() {
    let d = scratch("krel");
    let o = run(
        &["--staging", d.to_str().unwrap(), "--kernel-dir", DRY_KDIR],
        &[],
    );
    fails_with(&o, "KERNEL_DIR does not exist");
}

#[test]
fn make_expression_in_kernel_arch_is_rejected() {
    let d = scratch("karch");
    let os = vendor();
    let st = d.to_str().unwrap();
    for bad in ["$(shell touch pwned)", "x86;y", "a b"] {
        let o = run(&with(dry_args(st, &os), &["--kernel-arch", bad]), &[]);
        fails_with(&o, "invalid KERNEL_ARCH");
    }
    let o = run(&with(dry_args(st, &os), &["--kernel-arch", "aarch64"]), &[]);
    fails_with(&o, "only x86_64 is supported");
    assert!(!d.join("pwned").exists());
}

#[test]
fn staging_must_be_absolute_and_absent_or_empty() {
    let d = scratch("stage");
    let os = vendor();
    fails_with(
        &run(&dry_args("rel/stage", &os), &[]),
        "--staging must be absolute",
    );
    std::fs::write(d.join("junk"), "x").unwrap();
    fails_with(
        &run(&dry_args(d.to_str().unwrap(), &os), &[]),
        "must be absent or empty",
    );
    let f = d.join("junk");
    fails_with(
        &run(&dry_args(f.to_str().unwrap(), &os), &[]),
        "not a directory",
    );
    // Absent is fine and stays absent in dry-run.
    let absent = d.join("new");
    let o = run(&dry_args(absent.to_str().unwrap(), &os), &[]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(!absent.exists());
}

#[test]
fn opensnitch_dir_must_be_a_git_checkout_even_in_dry_run() {
    let d = scratch("nogit");
    let src = d.join("os");
    std::fs::create_dir_all(src.join("ebpf_prog")).unwrap();
    let st = d.join("s");
    let o = run(&dry_args(st.to_str().unwrap(), src.to_str().unwrap()), &[]);
    fails_with(&o, "not a git checkout");
}

#[test]
fn release_pin_mismatch_fails_clearly() {
    let d = scratch("relpin");
    let s = layout(
        &d,
        &[],
        &[(
            "SW_OPENSNITCH_COMMIT",
            "0000000000000000000000000000000000000000",
        )],
    );
    let st = d.join("s");
    let o = run_script(&s, &dry_args(st.to_str().unwrap(), &vendor()), &[]);
    fails_with(&o, "commit pin mismatch");
}

#[test]
fn checkout_commit_mismatch_fails_clearly() {
    let d = scratch("pins");
    let zero = "0000000000000000000000000000000000000000";
    let s = layout(
        &d,
        &[("SW_OPENSNITCH_COMMIT", zero)],
        &[("SW_OPENSNITCH_COMMIT", zero)],
    );
    let st = d.join("s");
    let o = run_script(&s, &dry_args(st.to_str().unwrap(), &vendor()), &[]);
    fails_with(&o, "commit pin mismatch");
}

#[test]
fn malformed_pins_are_rejected() {
    let up = "A1353848BA1B660320E90CEFEA782C3FBA272C00";
    let cases: &[(&str, &str)] = &[
        ("SW_OPENSNITCH_COMMIT", "a135"),
        ("SW_OPENSNITCH_COMMIT", up),
        ("SW_EBPF_CLANG_MAJOR", "21a"),
        ("SW_EBPF_LLVM_MAJOR", ""),
        ("SW_EBPF_LLC_MAJOR", "$(id)"),
        ("SW_EBPF_PROG_TREE_SHA256", "abc"),
        ("SW_EBPF_KERNEL_DEVEL_VERSION", "7"),
    ];
    for (i, (k, v)) in cases.iter().enumerate() {
        let d = scratch(&format!("badpin{i}"));
        let s = layout(&d, &[(k, v)], &[(k, v)]);
        let st = d.join("s");
        let o = run_script(&s, &dry_args(st.to_str().unwrap(), &vendor()), &[]);
        assert!(!o.status.success(), "{k}={v}: {}", text(&o));
        assert!(
            text(&o).contains(k) || text(&o).contains("KERNEL_VER"),
            "{k}={v}: {}",
            text(&o)
        );
    }
}

fn run_repo(d: &Path, r: &Path, commit: &str, tree: &str) -> Output {
    let s = layout(
        d,
        &[
            ("SW_OPENSNITCH_COMMIT", commit),
            ("SW_EBPF_PROG_TREE_SHA256", tree),
        ],
        &[("SW_OPENSNITCH_COMMIT", commit)],
    );
    let st = d.join("s");
    run_script(
        &s,
        &dry_args(st.to_str().unwrap(), r.to_str().unwrap()),
        &[],
    )
}

#[test]
fn tampered_tree_fails_the_tree_hash_pin() {
    let d = scratch("tamper");
    let (r, head) = repo(&d, SRC, &[]);
    let real_pin = parse_env(&read(PINS))["SW_EBPF_PROG_TREE_SHA256"].clone();
    fails_with(&run_repo(&d, &r, &head, &real_pin), "tree hash mismatch");
}

#[test]
fn tree_hash_follows_raw_blobs_not_git_attributes_or_filters() {
    let d = scratch("attrs");
    let raw = d.join("raw");
    for (p, c) in SRC {
        let f = raw.join(p);
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(f, c).unwrap();
    }
    let expected = tree_hash(&raw);
    let marker = d.join("pwned");
    let attrs = "* export-subst ident filter=evil\nbpf_headers/** export-ignore\n";
    let (r, head) = repo(&d, SRC, &[(".gitattributes", attrs)]);
    std::fs::write(r.join("ebpf_prog/.gitattributes"), attrs).unwrap();
    std::fs::create_dir_all(r.join(".git/info")).unwrap();
    std::fs::write(r.join(".git/info/attributes"), attrs).unwrap();
    let smudge = format!("touch {}; cat", marker.display());
    git(&r, &["config", "filter.evil.smudge", &smudge]);
    git(&r, &["config", "filter.evil.required", "true"]);
    let o = run_repo(&d, &r, &head, &expected);
    assert!(o.status.success(), "{}", text(&o));
    assert!(text(&o).contains("tree hash ok"), "{}", text(&o));
    assert!(!marker.exists(), "a git filter ran");
}

#[test]
fn prebuilt_objects_and_symlinks_in_source_are_refused() {
    for (i, name) in ["opensnitch.o", "x.bc", "bpf_headers/y.o"]
        .iter()
        .enumerate()
    {
        let d = scratch(&format!("obj{i}"));
        let mut files = SRC.to_vec();
        files.push((name, "ELF"));
        let (r, head) = repo(&d, &files, &[]);
        fails_with(&run_repo(&d, &r, &head, &"0".repeat(64)), "prebuilt object");
    }
    let d = scratch("symlink");
    let (r, _) = repo(&d, SRC, &[]);
    std::os::unix::fs::symlink("/etc/passwd", r.join("ebpf_prog/common.h")).unwrap();
    git(&r, &["add", "-A"]);
    git(&r, &["commit", "-q", "-m", "link"]);
    let head = git(&r, &["rev-parse", "HEAD"]);
    fails_with(
        &run_repo(&d, &r, &head, &"0".repeat(64)),
        "not a regular file",
    );
}

fn drop_blob(r: &Path, rel: &str) {
    let sha = git(r, &["rev-parse", &format!("HEAD:ebpf_prog/{rel}")]);
    std::fs::remove_file(r.join(".git/objects").join(&sha[..2]).join(&sha[2..])).unwrap();
}

#[test]
fn command_running_local_git_config_is_refused_before_any_fetch() {
    let d = scratch("promisor");
    let marker = d.join("pwned");
    let touch = format!("touch {}; false", marker.display());
    let inc = d.join("inc.cfg");
    std::fs::write(&inc, format!("[core]\n\tsshCommand = {touch}\n")).unwrap();
    let inc = inc.to_str().unwrap().to_string();
    let cases: &[&[(&str, &str)]] = &[
        &[
            ("core.repositoryformatversion", "1"),
            ("extensions.partialClone", "origin"),
            ("remote.origin.url", "ssh://example.invalid/x"),
            ("remote.origin.promisor", "true"),
            ("remote.origin.uploadpack", &touch),
            ("core.sshCommand", &touch),
        ],
        &[("core.sshCommand", &touch)],
        &[("remote.origin.promisor", "true")],
        &[("include.path", &inc)],
    ];
    for (i, cfg) in cases.iter().enumerate() {
        let d = scratch(&format!("promisor{i}"));
        let (r, head) = repo(&d, SRC, &[]);
        drop_blob(&r, "bpf_headers/a.h");
        for (k, v) in cfg.iter() {
            git(&r, &["config", k, v]);
        }
        let o = run_repo(&d, &r, &head, &"0".repeat(64));
        fails_with(&o, "repository-local git config");
        assert!(!marker.exists(), "{cfg:?} ran a command");
    }
}

#[test]
fn missing_blob_is_refused_without_fetching() {
    let d = scratch("missing");
    let (r, head) = repo(&d, SRC, &[]);
    drop_blob(&r, "bpf_headers/a.h");
    fails_with(
        &run_repo(&d, &r, &head, &"0".repeat(64)),
        "object not present locally",
    );
}

#[test]
fn kernel_ver_is_tied_to_the_pin() {
    let d = scratch("kpin");
    let os = vendor();
    let st = d.to_str().unwrap();
    let kdir = "/usr/src/kernels/7.2.8-100.fc43.x86_64";
    let args = |extra: &[&'static str]| {
        let mut a = vec![
            "--dry-run",
            "--staging",
            st,
            "--kernel-dir",
            kdir,
            "--opensnitch-dir",
            &os,
            "--kernel-ver",
            "7.2.8",
        ];
        a.extend_from_slice(extra);
        a
    };
    fails_with(&run(&args(&[]), &[]), "does not match the pin");
    let o = run(&args(&["--allow-kernel-mismatch"]), &[]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(
        text(&o).contains("WARNING: --allow-kernel-mismatch"),
        "{}",
        text(&o)
    );
    // The pinned value itself needs no override.
    let o = run(&with(dry_args(st, &os), &["--kernel-ver", "7.2.15"]), &[]);
    assert!(o.status.success(), "{}", text(&o));
}

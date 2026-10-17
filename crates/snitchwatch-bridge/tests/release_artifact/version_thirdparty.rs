use super::*;

// ------------------------------------------------------------------ version / thirdparty

#[test]
fn version_prints_the_bridge_cli_version_resolving_the_workspace_version() {
    let out = run_ok(
        artifact_tool()
            .args(["version", "--repo"])
            .arg(workspace_file("")),
    );
    assert_eq!(out, format!("{VERSION}\n"), "the real workspace");

    let repo = tempfile::tempdir().expect("tempdir");
    let crate_toml = repo.path().join("crates/snitchwatch-bridge-cli/Cargo.toml");
    write_file(
        &crate_toml,
        b"[package]\nname = \"snitchwatch-bridge-cli\"\nversion.workspace = true\n",
        0o644,
    );
    let workspace_toml = repo.path().join("Cargo.toml");
    for (version, ok) in [("1.2.3", true), ("1.2", false), ("1.2.3\\n", false)] {
        let body = format!("[workspace.package]\nversion = \"{version}\"\n");
        write_file(&workspace_toml, body.as_bytes(), 0o644);
        let res = run(artifact_tool().args(["version", "--repo"]).arg(repo.path()));
        assert_eq!(
            res.status.success(),
            ok,
            "{version}: {}",
            describe("version", &res)
        );
        if ok {
            assert_eq!(String::from_utf8_lossy(&res.stdout), format!("{version}\n"));
        } else {
            assert!(String::from_utf8_lossy(&res.stderr).contains("version"));
        }
    }
}

fn write_text(path: &Path, text: &str) {
    write_file(path, text.as_bytes(), 0o644);
}

fn path_crate(dir: &Path, name: &str, version: &str, extra: &str) {
    write_text(
        &dir.join("Cargo.toml"),
        &format!(
            "[package]\nname = \"{name}\"\nversion = \"{version}\"\nedition = \"2021\"\n{extra}"
        ),
    );
    write_text(&dir.join("src/lib.rs"), "");
}

/// A workspace (app = `snitchwatch-bridge-cli` + a second member) whose
/// third-party deps are path crates OUTSIDE the workspace root (inside it they
/// would become members). Needs no registry and no network.
fn thirdparty_fixture(root: &Path) -> PathBuf {
    let ws = root.join("ws");
    let third = root.join("third");
    write_text(
        &ws.join("Cargo.toml"),
        "[workspace]\nmembers = [\"app\", \"proto\", \"shell\"]\nresolver = \"2\"\n",
    );
    path_crate(
        &ws.join("app"),
        "snitchwatch-bridge-cli",
        "0.1.0",
        "[dependencies]\nsnitchwatch-proto = { path = \"../proto\" }\n\
         alpha = { path = \"../../third/alpha\" }\n\
         epsilon = { path = \"../../third/epsilon\", optional = true }\n\
         [features]\ndefault = [\"web-ui\"]\nweb-ui = [\"dep:epsilon\"]\n\
         [dev-dependencies]\ndevonly = { path = \"../../third/devonly\" }\n\
         [build-dependencies]\nbuildonly = { path = \"../../third/buildonly\" }\n",
    );
    // Another member turns on alpha's `extra` feature. Feature unification puts
    // delta into `cargo metadata`'s resolve, but the release build never has it.
    path_crate(
        &ws.join("shell"),
        "shell",
        "0.1.0",
        "[dependencies]\nalpha = { path = \"../../third/alpha\", features = [\"extra\"] }\n",
    );
    write_text(&ws.join("app/build.rs"), "fn main() {}\n");
    path_crate(
        &ws.join("proto"),
        "snitchwatch-proto",
        "0.1.0",
        "[dependencies]\nbeta = { path = \"../../third/beta\" }\n",
    );
    path_crate(
        &third.join("alpha"),
        "alpha",
        "1.0.0",
        "license = \"MIT OR Apache-2.0\"\nrepository = \"https://example.invalid/alpha\"\n\
         [dependencies]\ngamma = { path = \"../gamma\" }\n\
         delta = { path = \"../delta\", optional = true }\n\
         [features]\nextra = [\"dep:delta\"]\n",
    );
    write_text(
        &third.join("alpha/LICENSE-MIT"),
        "ALPHA-MIT-TEXT\n```\nfenced\n```\n",
    );
    write_text(&third.join("alpha/NOTICE"), "ALPHA-NOTICE-TEXT");
    write_text(
        &third.join("alpha/license-extra.txt"),
        "ALPHA-LOWERCASE-LICENSE\n",
    );
    write_text(
        &third.join("alpha/README.md"),
        "ALPHA-README-NOT-A-LICENSE\n",
    );
    path_crate(
        &third.join("beta"),
        "beta",
        "2.0.0",
        "license-file = \"COPYING\"\n",
    );
    write_text(&third.join("beta/COPYING"), "BETA-COPYING-TEXT\n");
    path_crate(
        &third.join("gamma"),
        "gamma",
        "0.3.0",
        "license = \"Unicode-3.0\"\n",
    );
    for name in ["devonly", "buildonly", "delta", "epsilon"] {
        path_crate(&third.join(name), name, "1.0.0", "license = \"MIT\"\n");
        write_text(&third.join(name).join("LICENSE"), &format!("{name}-TEXT\n"));
    }
    run_ok(
        Command::new(env!("CARGO"))
            .args(["generate-lockfile", "--offline", "--manifest-path"])
            .arg(ws.join("Cargo.toml")),
    );
    ws
}

fn thirdparty(ws: &Path, out: &Path) -> String {
    let cargo_dir = Path::new(env!("CARGO"))
        .parent()
        .expect("cargo has a parent dir");
    let path = format!(
        "{}:{}",
        cargo_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    run_ok(
        artifact_tool()
            .env("PATH", path)
            .args(["thirdparty", "--repo"])
            .arg(ws)
            .arg("--out")
            .arg(out),
    );
    fs::read_to_string(out).expect("read THIRD-PARTY-LICENSES.md")
}

#[test]
fn thirdparty_lists_the_normal_dependency_closure_deterministically() {
    let a = tempfile::tempdir().expect("tempdir");
    let b = tempfile::tempdir().expect("tempdir");
    let text = thirdparty(&thirdparty_fixture(a.path()), &a.path().join("out.md"));
    let again = thirdparty(&thirdparty_fixture(b.path()), &b.path().join("out.md"));
    assert_eq!(text, again, "output must not depend on the checkout path");

    let headings: Vec<&str> = text.lines().filter(|l| l.starts_with("## ")).collect();
    assert_eq!(
        headings,
        ["## alpha 1.0.0", "## beta 2.0.0", "## gamma 0.3.0"],
        "normal closure only (through workspace members, which are not listed)\n{text}"
    );
    // delta: only another member's feature enables it; epsilon: only the
    // default feature that the release build's --no-default-features drops.
    for absent in [
        "devonly",
        "buildonly",
        "delta",
        "epsilon",
        "README-NOT-A-LICENSE",
    ] {
        assert!(
            !text.contains(absent),
            "{absent} must not be listed\n{text}"
        );
    }
    for present in [
        "- License (SPDX): MIT OR Apache-2.0",
        "- Repository: <https://example.invalid/alpha>",
        "ALPHA-MIT-TEXT\n```\nfenced\n```\n````",
        "ALPHA-NOTICE-TEXT",
        "ALPHA-LOWERCASE-LICENSE",
        "- License (SPDX): not declared in Cargo.toml",
        "- License file (declared): `COPYING`",
        "BETA-COPYING-TEXT",
        "- License (SPDX): Unicode-3.0",
        "ships no LICENSE*",
    ] {
        assert!(text.contains(present), "missing {present:?}\n{text}");
    }
    for root in [a.path(), b.path()] {
        let root = root.to_string_lossy();
        assert!(
            !text.contains(&*root),
            "absolute path {root} leaked\n{text}"
        );
    }
}

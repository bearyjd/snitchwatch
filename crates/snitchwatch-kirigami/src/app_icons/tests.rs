use super::*;
use std::sync::Arc;

fn write(dir: &Path, name: &str, body: &str) {
    std::fs::write(dir.join(name), body).unwrap();
}

fn entry(exec: &str, icon: &str) -> String {
    format!("[Desktop Entry]\nType=Application\nName=X\nExec={exec}\nIcon={icon}\n")
}

#[test]
fn exec_tokens_handle_quotes_and_escapes() {
    assert_eq!(
        exec_tokens(r#""/opt/My App/run" --flag "a b" c\ d"#),
        ["/opt/My App/run", "--flag", "a b", "c d"]
    );
}

#[test]
fn exec_program_skips_env_and_assignments() {
    let t = exec_tokens("env FOO=1 BAR=2 /usr/bin/thing %U");
    assert_eq!(exec_program(&t), Some("/usr/bin/thing"));
}

#[test]
fn parses_exec_basename_and_icon() {
    let e = parse_desktop(&entry("firefox %u", "firefox"), "firefox").unwrap();
    assert_eq!(e.programs, ["firefox"]);
    assert_eq!(e.icon, "firefox");
    assert_eq!(e.flatpak_id, None);
}

#[test]
fn ignores_other_groups_and_non_applications() {
    let body = "[Desktop Entry]\nType=Application\nExec=a\nIcon=right\n\
                [Desktop Action x]\nExec=b\nIcon=wrong\n";
    assert_eq!(parse_desktop(body, "a").unwrap().icon, "right");
    let link = "[Desktop Entry]\nType=Link\nExec=a\nIcon=i\n";
    assert!(parse_desktop(link, "a").is_none());
    assert!(parse_desktop("[Desktop Entry]\nType=Application\nExec=a\n", "a").is_none());
}

#[test]
fn flatpak_run_exec_yields_id_not_flatpak_program() {
    let e = parse_desktop(
        &entry(
            "/usr/bin/flatpak run --branch=stable --command=fx org.mozilla.firefox @@u %u @@",
            "org.mozilla.firefox",
        ),
        "org.mozilla.firefox",
    )
    .unwrap();
    assert_eq!(e.flatpak_id.as_deref(), Some("org.mozilla.firefox"));
    assert!(e.programs.is_empty());
}

#[test]
fn x_flatpak_key_gives_id() {
    let body = "[Desktop Entry]\nType=Application\nExec=thing\nIcon=i\nX-Flatpak=org.a.B\n";
    assert_eq!(
        parse_desktop(body, "other").unwrap().flatpak_id.as_deref(),
        Some("org.a.B")
    );
}

#[test]
fn flatpak_id_from_host_install_path() {
    let p =
        "/var/lib/flatpak/app/org.mozilla.firefox/x86_64/stable/abc123/files/lib/firefox/firefox";
    assert_eq!(
        flatpak_app_id_from_path(p, None).as_deref(),
        Some("org.mozilla.firefox")
    );
    let u = "/home/u/.local/share/flatpak/app/com.valve.Steam/x86_64/stable/c/files/bin/steam";
    assert_eq!(
        flatpak_app_id_from_path(u, Some("/home/u")).as_deref(),
        Some("com.valve.Steam")
    );
    assert_eq!(flatpak_app_id_from_path("/app/bin/foo", None), None);
    assert_eq!(flatpak_app_id_from_path("/usr/bin/bwrap", None), None);
}

#[test]
fn flatpak_id_only_from_the_real_install_locations() {
    let home = Some("/home/u");
    for p in [
        "/home/u/flatpak/app/org.mozilla.firefox/evil",
        "/tmp/flatpak/app/org.mozilla.firefox/x86_64/stable/c/files/bin/x",
        "/home/other/.local/share/flatpak/app/org.mozilla.firefox/x/y",
        "/var/lib/flatpak/app/org.mozilla.firefox",
        "/var/lib/flatpak/app/org.mozilla.firefox/../../../../tmp/x",
        "/var/lib/flatpak/app/not-an-id/x",
    ] {
        assert_eq!(flatpak_app_id_from_path(p, home), None, "{p}");
    }
    // The per-user location needs a known absolute $HOME.
    let u = "/home/u/.local/share/flatpak/app/com.valve.Steam/x/y";
    assert_eq!(flatpak_app_id_from_path(u, None), None);
    assert_eq!(flatpak_app_id_from_path(u, Some("home/u")), None);
}

#[test]
fn index_matches_absolute_then_basename_then_falls_back() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "a.desktop",
        &entry("/usr/bin/firefox %u", "firefox-icon"),
    );
    write(dir.path(), "b.desktop", &entry("/opt/x/tool", "tool-icon"));
    write(
        dir.path(),
        "c.desktop",
        &entry("env A=1 syncthing-gtk", "st-icon"),
    );
    write(dir.path(), "notes.txt", "ignored");
    let idx = AppIconIndex::scan(&[dir.path().to_path_buf()]);
    assert_eq!(idx.icon_for("/usr/bin/firefox"), "firefox-icon");
    assert_eq!(idx.icon_for("/usr/libexec/firefox"), "firefox-icon");
    assert_eq!(idx.icon_for("/opt/x/tool"), "tool-icon");
    assert_eq!(idx.icon_for("/usr/bin/syncthing-gtk"), "st-icon");
    assert_eq!(idx.icon_for("/usr/sbin/unknownd"), FALLBACK_ICON);
    assert_eq!(idx.icon_for(""), FALLBACK_ICON);
}

#[test]
fn a_name_alone_matches_only_in_a_standard_bin_dir() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "a.desktop",
        &entry("firefox %u", "firefox-icon"),
    );
    let idx = AppIconIndex::scan(&[dir.path().to_path_buf()]);
    for dir in BIN_DIRS {
        assert_eq!(idx.icon_for(&format!("{dir}/firefox")), "firefox-icon");
    }
    for p in [
        // A sandbox-internal path names no host program.
        "/app/bin/firefox",
        "/tmp/x/firefox",
        "/home/u/Downloads/firefox",
        "/usr/lib64/firefox/firefox",
        "/usr/bin/sub/firefox",
        "firefox",
    ] {
        assert_eq!(idx.icon_for(p), FALLBACK_ICON, "{p}");
    }
}

#[test]
fn ready_hooks_run_once_when_ready_and_at_once_after() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let count = Arc::new(AtomicUsize::new(0));
    let hook = || {
        let count = count.clone();
        Box::new(move || {
            count.fetch_add(1, Ordering::SeqCst);
        }) as Box<dyn FnOnce() + Send>
    };
    let mut hooks = ReadyHooks::default();
    hooks.add(hook());
    assert_eq!(count.load(Ordering::SeqCst), 0);
    hooks.mark_ready();
    assert_eq!(count.load(Ordering::SeqCst), 1);
    hooks.mark_ready();
    assert_eq!(count.load(Ordering::SeqCst), 1);
    hooks.add(hook());
    assert_eq!(count.load(Ordering::SeqCst), 2);
}

#[test]
fn a_made_up_flatpak_path_gets_the_generic_icon() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "org.mozilla.firefox.desktop",
        &entry("/usr/bin/flatpak run org.mozilla.firefox", "ff-icon"),
    );
    let idx =
        AppIconIndex::scan(&[dir.path().to_path_buf()]).with_home(Some("/home/u".to_string()));
    assert_eq!(
        idx.icon_for("/home/u/flatpak/app/org.mozilla.firefox/evil"),
        FALLBACK_ICON
    );
    assert_eq!(
        idx.icon_for("/tmp/flatpak/app/org.mozilla.firefox/x/y/files/bin/f"),
        FALLBACK_ICON
    );
    assert_eq!(
        idx.icon_for("/home/u/.local/share/flatpak/app/org.mozilla.firefox/x/y/files/bin/f"),
        "ff-icon"
    );
}

#[test]
fn scan_skips_non_files_and_oversized_files_reads_symlinked_files() {
    let (apps, other) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    // A directory named like a desktop file.
    std::fs::create_dir(apps.path().join("dir.desktop")).unwrap();
    // A symlink to a regular file is fine.
    write(
        other.path(),
        "real.desktop",
        &entry("/usr/bin/linked", "linked-icon"),
    );
    std::os::unix::fs::symlink(
        other.path().join("real.desktop"),
        apps.path().join("link.desktop"),
    )
    .unwrap();
    // A dangling symlink is skipped.
    std::os::unix::fs::symlink(
        other.path().join("missing.desktop"),
        apps.path().join("dangling.desktop"),
    )
    .unwrap();
    // The program line sits past the read cap, so the entry is unusable.
    let mut big = "[Desktop Entry]\nType=Application\nIcon=big-icon\n".to_string();
    big.push_str(&"# padding\n".repeat(MAX_DESKTOP_BYTES as usize / 8));
    big.push_str("Exec=/usr/bin/big\n");
    write(apps.path(), "big.desktop", &big);
    let idx = AppIconIndex::scan(&[apps.path().to_path_buf()]);
    assert_eq!(idx.icon_for("/usr/bin/linked"), "linked-icon");
    assert_eq!(idx.icon_for("/usr/bin/big"), FALLBACK_ICON);
}

#[test]
fn only_theme_names_and_files_under_the_data_dirs_are_icons() {
    let share = tempfile::tempdir().unwrap();
    let apps = share.path().join("applications");
    std::fs::create_dir(&apps).unwrap();
    let inside = share.path().join("pixmaps-app.png");
    std::fs::write(&inside, b"png").unwrap();
    let outside = tempfile::tempdir().unwrap();
    let elsewhere = outside.path().join("evil.png");
    std::fs::write(&elsewhere, b"png").unwrap();
    // A link inside the data dir that points outside it.
    let link = share.path().join("link.png");
    std::os::unix::fs::symlink(&elsewhere, &link).unwrap();
    let cases = [
        ("theme-icon", "/usr/bin/a", true),
        ("org.app.Name", "/usr/bin/b", true),
        (inside.to_str().unwrap(), "/usr/bin/c", true),
        (elsewhere.to_str().unwrap(), "/usr/bin/d", false),
        (link.to_str().unwrap(), "/usr/bin/e", false),
        ("/nonexistent/under/nothing.png", "/usr/bin/f", false),
        ("https://example.com/i.png", "/usr/bin/g", false),
        ("file:///etc/passwd", "/usr/bin/h", false),
        ("relative/icon.png", "/usr/bin/i", false),
        ("../x", "/usr/bin/j", false),
    ];
    for (n, (icon, program, _)) in cases.iter().enumerate() {
        write(&apps, &format!("{n}.desktop"), &entry(program, icon));
    }
    let idx = AppIconIndex::scan(&[apps]);
    for (icon, program, kept) in cases {
        let got = idx.icon_for(program);
        if kept {
            assert_eq!(got, icon, "{icon}");
        } else {
            assert_eq!(got, FALLBACK_ICON, "{icon} must be rejected");
        }
    }
}

#[test]
fn earlier_directory_wins() {
    let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    write(a.path(), "x.desktop", &entry("prog", "user-icon"));
    write(b.path(), "x.desktop", &entry("prog", "system-icon"));
    let idx = AppIconIndex::scan(&[a.path().into(), b.path().into()]);
    assert_eq!(idx.icon_for("/usr/bin/prog"), "user-icon");
}

#[test]
fn flatpak_install_path_matches_by_app_id_only() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "org.mozilla.firefox.desktop",
        &entry(
            "/usr/bin/flatpak run org.mozilla.firefox",
            "org.mozilla.firefox",
        ),
    );
    let idx = AppIconIndex::scan(&[dir.path().to_path_buf()]);
    let p = "/var/lib/flatpak/app/org.mozilla.firefox/x86_64/stable/h/files/bin/firefox";
    assert_eq!(idx.icon_for(p), "org.mozilla.firefox");
    // No derivable id: generic icon, and `flatpak` itself never matches.
    assert_eq!(idx.icon_for("/app/bin/firefox"), FALLBACK_ICON);
    assert_eq!(idx.icon_for("/usr/bin/flatpak"), FALLBACK_ICON);
    assert_eq!(idx.icon_for("/usr/bin/bwrap"), FALLBACK_ICON);
}

#[test]
fn search_dirs_orders_user_then_xdg_then_flatpak_then_host() {
    let env = |k: &str| match k {
        "HOME" => Some("/home/u".to_string()),
        "XDG_DATA_DIRS" => Some("/opt/share:/usr/share".to_string()),
        _ => None,
    };
    let dirs = search_dirs(env);
    let s: Vec<String> = dirs.iter().map(|d| d.display().to_string()).collect();
    assert_eq!(s[0], "/home/u/.local/share/applications");
    assert_eq!(s[1], "/opt/share/applications");
    assert!(s.contains(&"/var/lib/flatpak/exports/share/applications".to_string()));
    assert!(s.contains(&"/home/u/.local/share/flatpak/exports/share/applications".to_string()));
    assert!(s.contains(&"/run/host/usr/share/applications".to_string()));
    assert_eq!(
        s.iter().filter(|d| *d == "/usr/share/applications").count(),
        1
    );
}

#[test]
fn xdg_data_home_comes_first_then_the_home_data_dir() {
    let env = |k: &str| match k {
        "HOME" => Some("/home/u".to_string()),
        "XDG_DATA_HOME" => Some("/data".to_string()),
        _ => None,
    };
    let dirs = search_dirs(env);
    assert_eq!(dirs[0], PathBuf::from("/data/applications"));
    assert_eq!(dirs[1], PathBuf::from("/home/u/.local/share/applications"));
}

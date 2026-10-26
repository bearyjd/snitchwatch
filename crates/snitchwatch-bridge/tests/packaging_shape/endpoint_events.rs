use super::*;

/// The opt-in recipe for sharing connection events with Vigie
/// (docs/superpowers/plans/2026-10-26-endpoint-events-packaging.md). Nothing
/// here is installed by default; these tests pin what the recipe forwards and
/// that it stays opt-in. `packaging/endpoint-events/verify.sh` runs the real
/// rsyslog end to end; these are the cheap, always-on guards.
const DIR: &str = "packaging/endpoint-events";
const LOGGER: &str = "packaging/endpoint-events/opensnitchd-logger.json";
const RSYSLOG: &str = "packaging/endpoint-events/snitchwatch-endpoint-events.conf";
const README: &str = "packaging/endpoint-events/README.md";

/// Everything the forwarded template may name: the spec's allowlist plus
/// the configured host id. rsyslog lowercases outnames, so the daemon's
/// `Action`/`Rule`/`Type` are forwarded as `action`/`rule`/`type`.
const ALLOWED_FIELDS: [&str; 13] = [
    "time",
    "protocol",
    "src_ip",
    "src_port",
    "dst_ip",
    "dst_port",
    "dst_host",
    "process_path",
    "user_id",
    "action",
    "rule",
    "type",
    "host_id",
];

/// Connection fields that must never leave the computer: environment and
/// arguments can hold tokens and passwords.
const FORBIDDEN_FIELDS: [&str; 5] = [
    "process_env",
    "process_args",
    "process_cwd",
    "process_tree",
    "process_checksums",
];

/// The drop-in's active text: `#` comments and blank lines removed. The
/// drop-in deliberately uses only `#` comments, so this is all a comment is.
fn rsyslog_active() -> String {
    read(RSYSLOG)
        .lines()
        .map(|line| match line.find('#') {
            Some(at) => &line[..at],
            None => line,
        })
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The `name(...)` statements (`input`, `action`, `template`, ...) of the
/// drop-in, each with its parenthesised body. Strings in the drop-in hold no
/// parentheses, so nesting is plain counting.
fn statements(active: &str, name: &str) -> Vec<String> {
    let opener = format!("{name}(");
    let mut found = Vec::new();
    let mut rest = active;
    while let Some(at) = rest.find(&opener) {
        let preceded_by_word = rest[..at]
            .chars()
            .last()
            .is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '.');
        let body_start = at + opener.len();
        if preceded_by_word {
            rest = &rest[body_start..];
            continue;
        }
        let mut depth = 1;
        let mut end = body_start;
        for (offset, c) in rest[body_start..].char_indices() {
            match c {
                '(' => depth += 1,
                ')' => depth -= 1,
                _ => {}
            }
            if depth == 0 {
                end = body_start + offset;
                break;
            }
        }
        assert_eq!(depth, 0, "unbalanced `{name}(` in {RSYSLOG}");
        found.push(rest[body_start..end].to_string());
        rest = &rest[end..];
    }
    found
}

/// `key="value"` from a statement body, matching the key case-insensitively
/// (rsyslog parameter names are case-insensitive).
fn param(body: &str, key: &str) -> Option<String> {
    let lower = body.to_ascii_lowercase();
    let needle = format!("{}=\"", key.to_ascii_lowercase());
    let mut search = 0;
    while let Some(found) = lower[search..].find(&needle) {
        let at = search + found;
        let boundary = lower[..at]
            .chars()
            .last()
            .is_none_or(|c| !(c.is_alphanumeric() || c == '.' || c == '_'));
        let value_start = at + needle.len();
        if boundary {
            let len = body[value_start..].find('"').expect("unterminated value");
            return Some(body[value_start..value_start + len].to_string());
        }
        search = value_start;
    }
    None
}

#[test]
fn statement_and_param_helpers_ignore_lookalikes() {
    let active = "module(load=\"imtcp\")\nxinput(a=\"1\")\ninput(type=\"imtcp\" Address=\"127.0.0.1\" x.address=\"0.0.0.0\")";
    let inputs = statements(active, "input");
    assert_eq!(inputs.len(), 1);
    assert_eq!(param(&inputs[0], "address").as_deref(), Some("127.0.0.1"));
    assert_eq!(param(&inputs[0], "missing"), None);
}

/// The daemon side: a `remote` logger writing escaped JSON lines over TCP to
/// loopback port 514. Field names match `LoggerConfig`
/// (vendor/opensnitch/daemon/log/loggers/logger.go) and the entry is meant
/// for `Server.Loggers` (ui/config/config.go). Below 1024 so an unprivileged
/// local process can't bind the port first and receive the stream (#35).
/// The JSON format ignores `Tag`; it's set for the RFC 5424 case only, so
/// nothing downstream may filter on it.
#[test]
fn logger_entry_is_remote_json_over_tcp_to_a_privileged_loopback_port() {
    let json = daemon_config(LOGGER);
    let entry = json.as_object().expect("logger entry is one JSON object");
    let known = [
        "Name",
        "Format",
        "Protocol",
        "Server",
        "WriteTimeout",
        "ConnectTimeout",
        "Tag",
        "Workers",
        "MaxConnectAttempts",
    ];
    for key in entry.keys() {
        assert!(
            known.contains(&key.as_str()),
            "`{key}` is not a LoggerConfig field"
        );
    }
    assert_eq!(json["Name"], "remote");
    assert_eq!(json["Format"], "json");
    assert_eq!(json["Protocol"], "tcp");
    assert!(json["Tag"].as_str().is_some_and(|tag| !tag.is_empty()));

    let server = json["Server"].as_str().expect("Server is a string");
    let (host, port) = server.rsplit_once(':').expect("Server is host:port");
    assert_eq!(host, "127.0.0.1", "the logger is plain TCP: loopback only");
    let port: u16 = port.parse().expect("numeric port");
    assert!(port < 1024, "port {port} is squattable (#35)");
}

/// Every listener the drop-in opens binds 127.0.0.1, on the logger's port,
/// in its own ruleset; and the drop-in opens no listener any other way.
#[test]
fn every_rsyslog_input_binds_loopback_only() {
    let active = rsyslog_active();
    let lower = active.to_ascii_lowercase();
    for legacy in ["$inputtcpserverrun", "$udpserverrun", "$inputptcpserverrun"] {
        assert!(!lower.contains(legacy), "legacy listener `{legacy}`");
    }
    for module in ["imudp", "imptcp", "imrelp", "imgssapi"] {
        assert!(
            !lower.contains(module),
            "unexpected input module `{module}`"
        );
    }

    let inputs = statements(&active, "input");
    assert!(!inputs.is_empty(), "the drop-in must open its listener");
    let logger_port = daemon_config(LOGGER)["Server"]
        .as_str()
        .and_then(|s| s.rsplit_once(':'))
        .map(|(_, port)| port.to_string())
        .unwrap();
    let rulesets: Vec<String> = statements(&active, "ruleset")
        .iter()
        .filter_map(|body| param(body, "name"))
        .collect();
    for input in inputs {
        assert_eq!(param(&input, "type").as_deref(), Some("imtcp"), "{input}");
        assert_eq!(
            param(&input, "address").as_deref(),
            Some("127.0.0.1"),
            "input must bind loopback only: {input}"
        );
        assert_eq!(param(&input, "port"), Some(logger_port.clone()), "{input}");
        let ruleset = param(&input, "ruleset").expect("input has its own ruleset");
        assert!(
            rulesets.contains(&ruleset),
            "ruleset `{ruleset}` not defined"
        );
    }
}

/// The only action that sends anything off the computer is a TLS forward
/// that authenticates the server against a pinned CA, and the ruleset stops
/// afterwards so the events don't reach the local journal or files.
#[test]
fn forward_requires_tls_with_an_authenticated_peer_and_then_stops() {
    let active = rsyslog_active();
    let actions = statements(&active, "action");
    let forwards: Vec<&String> = actions
        .iter()
        .filter(|body| param(body, "type").as_deref() == Some("omfwd"))
        .collect();
    assert_eq!(forwards.len(), 1, "exactly one forward");
    // The only other action is the parser; no other output (files, journal).
    for body in &actions {
        let kind = param(body, "type").unwrap_or_default();
        assert!(kind == "omfwd" || kind == "mmjsonparse", "action `{kind}`");
    }
    let fwd = forwards[0];
    assert_eq!(param(fwd, "protocol").as_deref(), Some("tcp"));
    let driver = param(fwd, "StreamDriver").expect("StreamDriver set");
    assert!(driver == "gtls" || driver == "ossl", "driver `{driver}`");
    assert_eq!(param(fwd, "StreamDriverMode").as_deref(), Some("1"));
    // The server is pinned: by name under the pinned CA, or by fingerprint.
    // `x509/certvalid` alone accepts any certificate the CA signed, and
    // `anon` none at all.
    let auth = param(fwd, "StreamDriverAuthMode").expect("auth mode set");
    let peers = param(fwd, "StreamDriverPermittedPeers").unwrap_or_default();
    let peers: Vec<&str> = peers.split(',').map(str::trim).collect();
    assert!(
        peers.iter().all(|peer| !peer.is_empty()),
        "StreamDriverPermittedPeers must name the server"
    );
    match auth.as_str() {
        "x509/name" => {}
        "x509/fingerprint" => assert!(
            peers.iter().all(|peer| peer.starts_with("SHA")),
            "fingerprint mode needs SHA fingerprints, got {peers:?}"
        ),
        other => panic!("auth mode `{other}` doesn't pin the server"),
    }
    let file_param = |key: &str| param(fwd, key).filter(|path| path.starts_with('/'));
    assert!(file_param("streamDriver.CAFile").is_some(), "pinned CA");
    // Mutual TLS (Vigie's receiver contract): this computer presents its own
    // certificate, which is how Vigie identifies it.
    assert!(
        file_param("streamDriver.CertFile").is_some(),
        "client certificate"
    );
    assert!(file_param("streamDriver.KeyFile").is_some(), "client key");
    assert_eq!(
        param(fwd, "template").as_deref(),
        Some("snitchwatch_endpoint_event")
    );

    let after = &active[active.find("type=\"omfwd\"").unwrap()..];
    assert!(
        after.lines().any(|line| line.trim() == "stop"),
        "the ruleset must `stop` after forwarding"
    );
    assert!(
        param(fwd, "queue.type").is_some_and(|t| t.eq_ignore_ascii_case("LinkedList")),
        "bounded in-memory queue"
    );
    assert!(param(fwd, "queue.size").is_some(), "queue is bounded");
    assert!(
        param(fwd, "queue.filename").is_none(),
        "no disk-assisted queue: it would write the raw event, env included, to disk"
    );
}

/// The forwarded template names only allowlisted fields, and no forbidden
/// field appears anywhere outside comments.
#[test]
fn template_forwards_only_the_privacy_allowlist() {
    let active = rsyslog_active();
    for field in FORBIDDEN_FIELDS {
        assert!(
            !active.contains(field),
            "`{field}` appears outside a comment in {RSYSLOG}"
        );
    }

    // A list template is `template(...) { property(...) ... }`: take the
    // header through the closing brace of its block.
    let start = active
        .find("template(name=\"snitchwatch_endpoint_event\"")
        .expect("the forward's template is defined");
    let len = active[start..].find('}').expect("template block is closed");
    let template = &active[start..start + len];
    assert_eq!(statements(&active, "template").len(), 1, "one template");
    let seen: Vec<String> = template
        .match_indices("outname=\"")
        .map(|(at, needle)| {
            let value = &template[at + needle.len()..];
            value[..value.find('"').unwrap()].to_string()
        })
        .collect();
    assert!(!seen.is_empty(), "template renders named fields");
    for name in &seen {
        assert!(
            ALLOWED_FIELDS.contains(&name.as_str()),
            "template renders non-allowlisted `{name}`"
        );
    }
    // Every `$!` property the template reads is an allowlisted one.
    for (at, _) in template.match_indices("$!") {
        let path: String = template[at..].chars().take_while(|c| *c != '"').collect();
        let leaf = path.rsplit('!').next().unwrap().to_ascii_lowercase();
        assert!(
            ALLOWED_FIELDS.contains(&leaf.as_str()),
            "template reads non-allowlisted `{path}`"
        );
    }
}

/// Vigie drops frames over 8 KiB. Input accepts the daemon's large raw lines
/// (environment included) so they parse rather than truncate, and the
/// template caps every variable-length field so the worst-case output (each
/// capped byte escaped as `\u00XX`, six bytes) stays under 8 KiB.
#[test]
fn frames_are_bounded_for_vigie() {
    let active = rsyslog_active();
    let globals = statements(&active, "global");
    let max = globals
        .iter()
        .find_map(|body| param(body, "maxMessageSize"))
        .expect("maxMessageSize is set");
    assert_eq!(max, "64k", "input limit for the daemon's raw lines");

    let start = active
        .find("template(name=\"snitchwatch_endpoint_event\"")
        .unwrap();
    let template = &active[start..start + active[start..].find('}').unwrap()];
    let property = |outname: &str| {
        statements(template, "property")
            .into_iter()
            .find(|body| param(body, "outname").as_deref() == Some(outname))
            .unwrap_or_else(|| panic!("no `{outname}` property"))
    };
    let mut worst_case = 0;
    for (outname, cap) in [("process_path", 768), ("dst_host", 253), ("rule", 128)] {
        let to: usize = param(&property(outname), "position.to")
            .unwrap_or_else(|| panic!("`{outname}` is not capped"))
            .parse()
            .unwrap();
        assert_eq!(to, cap, "`{outname}` cap");
        worst_case += 6 * to;
    }
    // Everything else: keys, punctuation, time, IPs, numbers, action, a
    // 64-character host id (escaped), protocol.
    worst_case += 1024;
    assert!(worst_case < 8192, "worst-case frame {worst_case} bytes");
}

fn logger_configs(dir: &Path, found: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            logger_configs(&path, found);
        } else if path.extension().is_some_and(|ext| ext == "json") {
            found.push(path);
        }
    }
}

/// Opt-in, off by default: no shipped daemon config enables a logger, and
/// neither the image recipe nor any staging script installs the recipe or
/// rsyslog.
#[test]
fn nothing_shipped_enables_a_logger() {
    let opt_in = workspace_file(DIR).canonicalize().unwrap();
    let mut found = Vec::new();
    logger_configs(&workspace_file("packaging"), &mut found);
    for path in found {
        if path.canonicalize().unwrap().starts_with(&opt_in) {
            continue;
        }
        let Ok(json) =
            serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(&path).unwrap())
        else {
            continue;
        };
        let loggers = &json["Server"]["Loggers"];
        assert!(
            loggers.is_null() || loggers.as_array().is_some_and(Vec::is_empty),
            "{} enables a logger; the endpoint-events recipe is opt-in",
            path.display()
        );
    }
    for rel in [
        "packaging/bluebuild/recipe.yml",
        "packaging/system/stage.sh",
        "packaging/scanner/stage.sh",
    ] {
        let body = read(rel);
        for needle in ["Loggers", "rsyslog", "endpoint-events"] {
            assert!(!body.contains(needle), "{rel} mentions `{needle}`");
        }
    }
}

/// The README's procedure edits the 0600, root-only daemon config the same
/// way as "Restoring the allow default": built and validated as root, swapped
/// in only on success, and the daemon restarted by its Fedora unit name.
#[test]
fn readme_merges_atomically_as_root_and_restarts_the_fedora_unit() {
    let body = read(README);
    for needle in [
        "sudo sh -c 'set -e",
        "jq -e",
        "chmod 600",
        "mv \"$f.new\" \"$f\"",
        "/etc/opensnitchd/default-config.json",
        "rpm-ostree install",
        "rsyslog-mmjsonparse",
        "rsyslog-gnutls",
        "/usr/bin/rsyslogd",
        "host-cert.pem",
        "host-key.pem",
        "informational",
        "maxMessageSize",
    ] {
        assert!(body.contains(needle), "README missing `{needle}`");
    }
    let restarts: Vec<&str> = body
        .lines()
        .filter(|line| line.contains("systemctl restart"))
        .collect();
    assert!(!restarts.is_empty());
    assert!(
        restarts
            .iter()
            .any(|line| line.contains("opensnitch.service")),
        "the daemon restart must name opensnitch.service"
    );
    assert!(
        !body.contains("opensnitchd.service"),
        "the Fedora unit is opensnitch.service (issue #92)"
    );
}

/// The hand-off sample for Vigie is the line the podman proof's receiver got:
/// one JSON object with exactly the allowlisted keys.
#[test]
fn sample_event_has_exactly_the_allowlisted_fields() {
    let json = daemon_config("packaging/endpoint-events/sample-event.json");
    let mut keys: Vec<&str> = json
        .as_object()
        .expect("one JSON object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    let mut allowed = ALLOWED_FIELDS.to_vec();
    allowed.sort_unstable();
    assert_eq!(keys, allowed);
    let body = read("packaging/endpoint-events/sample-event.json");
    for field in FORBIDDEN_FIELDS {
        assert!(!body.contains(field), "sample carries `{field}`");
    }
}

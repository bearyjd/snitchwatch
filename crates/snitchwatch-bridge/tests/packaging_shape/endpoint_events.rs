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

/// The ruleset block (`ruleset(...) { ... }`) of the drop-in's active text.
fn ruleset_block(active: &str) -> &str {
    let start = active.find("ruleset(").expect("a ruleset");
    let body = &active[start..];
    let mut depth = 0;
    for (at, c) in body.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &body[..=at];
                }
            }
            _ => {}
        }
    }
    panic!("unclosed ruleset block");
}

/// The cap in `strlen(<property>) <= N` inside the ruleset, if any.
fn strlen_cap(ruleset: &str, property: &str) -> Option<usize> {
    let needle = format!("strlen({property}) <= ");
    let at = ruleset.find(&needle)? + needle.len();
    let digits: String = ruleset[at..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

/// The `$!` properties the forwarded template reads.
fn template_properties(active: &str) -> Vec<String> {
    let start = active
        .find("template(name=\"snitchwatch_endpoint_event\"")
        .expect("the forward's template is defined");
    let template = &active[start..start + active[start..].find('}').unwrap()];
    template
        .match_indices("name=\"$!")
        .map(|(at, needle)| {
            let value = &template[at + needle.len() - 2..];
            value[..value.find('"').unwrap()].to_string()
        })
        .collect()
}

/// Provenance and frame size (codex review of #233, HIGH): every field the
/// template forwards is validated in the ruleset (a pattern or a byte cap)
/// before the forward, so injected or oversize events are dropped. Nothing is
/// truncated, which could split a UTF-8 character.
#[test]
fn every_forwarded_field_is_validated_before_the_forward() {
    let active = rsyslog_active();
    let ruleset = ruleset_block(&active);
    let gate = &ruleset[..ruleset.find("type=\"omfwd\"").expect("a forward")];
    let properties = template_properties(&active);
    assert_eq!(properties.len(), 11, "{properties:?}");
    for property in &properties {
        let checked = gate.contains(&format!("re_match({property}, \"^"))
            || strlen_cap(gate, property).is_some();
        assert!(checked, "`{property}` is forwarded unvalidated");
    }
    assert!(
        !active.contains("position."),
        "no truncation: a byte cut can split a UTF-8 character"
    );
    assert!(gate.contains("if $parsesuccess != \"OK\" then stop"));
}

/// Vigie drops frames over 8 KiB. Input accepts the daemon's large raw lines
/// (environment included) so they parse rather than truncate; output caps
/// make the worst case (every capped byte escaped as `\u00XX`, six bytes)
/// fit.
#[test]
fn frames_are_bounded_for_vigie() {
    let active = rsyslog_active();
    let max = statements(&active, "global")
        .iter()
        .find_map(|body| param(body, "maxMessageSize"))
        .expect("maxMessageSize is set");
    assert_eq!(max, "64k", "input limit for the daemon's raw lines");

    let ruleset = ruleset_block(&active);
    let path = strlen_cap(ruleset, "$!Event!process_path").expect("path cap");
    let rule = strlen_cap(ruleset, "$!Rule").expect("rule cap");
    assert_eq!((path, rule), (768, 128));
    assert!(ruleset.contains("re_match($!Event!dst_host, \"^[A-Za-z0-9._:-]{0,253}\\$\")"));
    // Everything else is short by pattern: keys, punctuation, time, IPs,
    // numbers, protocol, action and a 64-character host id (escaped).
    let worst_case = 6 * (path + rule) + 253 + 1024;
    assert!(worst_case < 8192, "worst-case frame {worst_case} bytes");
}

/// Raw events (environment included) must not pass through rsyslog's main
/// queue, which a host may have made disk-assisted: the ruleset has its own
/// bounded, memory-only queue that is never saved (codex review, MEDIUM).
#[test]
fn ruleset_queue_is_bounded_and_memory_only() {
    let active = rsyslog_active();
    let ruleset = ruleset_block(&active);
    let header = &ruleset[..ruleset.find('{').unwrap()];
    assert!(
        param(header, "queue.type").is_some_and(|t| t.eq_ignore_ascii_case("LinkedList")),
        "ruleset queue: {header}"
    );
    assert!(param(header, "queue.size").is_some());
    assert_eq!(param(header, "queue.timeoutEnqueue").as_deref(), Some("0"));
    assert_eq!(
        param(header, "queue.saveOnShutdown").as_deref(),
        Some("off")
    );
    assert!(
        !active.to_ascii_lowercase().contains("queue.filename"),
        "no disk-assisted queue anywhere in the drop-in"
    );
}

const NFT: &str = "packaging/endpoint-events/snitchwatch-endpoint-events.nft";
const NFT_UNIT: &str = "packaging/endpoint-events/snitchwatch-endpoint-events-nft.service";

/// Only root may connect to the loopback listener (codex review, HIGH): a
/// separate nftables table rejects any non-root socket's connection to the
/// logger's port on 127.0.0.0/8 and ::1.
#[test]
fn nft_table_lets_only_root_connect_to_the_listener() {
    let body = read(NFT);
    let active: Vec<&str> = body
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect();
    let port = daemon_config(LOGGER)["Server"]
        .as_str()
        .and_then(|s| s.rsplit_once(':'))
        .map(|(_, port)| port.to_string())
        .unwrap();
    assert!(active.contains(&"table inet snitchwatch_endpoint_events {"));
    assert!(active
        .iter()
        .any(|line| line.starts_with("type filter hook output ")));
    for daddr in ["ip daddr 127.0.0.0/8", "ip6 daddr ::1"] {
        let rule = format!("{daddr} tcp dport {port} meta skuid != 0 reject");
        assert!(
            active.iter().any(|line| line.starts_with(&rule)),
            "missing `{rule}`"
        );
    }
    assert!(active
        .iter()
        .any(|line| line.starts_with("type filter hook input ")));
    for daddr in ["ip daddr 127.0.0.0/8", "ip6 daddr ::1"] {
        let rule = format!("iifname != \"lo\" {daddr} tcp dport {port} reject");
        assert!(
            active.iter().any(|line| line.starts_with(&rule)),
            "missing routed-packet rule `{rule}`"
        );
    }
    assert!(
        !active.iter().any(|line| line.contains("flush ruleset")),
        "never touch other tables (firewalld, opensnitchd)"
    );
}

const WORK_DIR: &str = "/var/lib/rsyslog/snitchwatch-endpoint-events";
const LISTENER_UNIT: &str = "packaging/endpoint-events/snitchwatch-endpoint-events.service";

/// The `Key=` values of a unit's active lines, split on whitespace.
fn unit_values(body: &str, key: &str) -> Vec<String> {
    let prefix = format!("{key}=");
    body.lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| line.strip_prefix(&prefix))
        .flat_map(|value| value.split_whitespace().map(str::to_string))
        .collect()
}

/// codex round 2, HIGH: the table's unit loads it, reloads it atomically
/// (never deleting it), and deletes it only on stop.
#[test]
fn nft_unit_loads_reloads_atomically_and_deletes_only_on_stop() {
    let body = read(NFT_UNIT);
    let load = "/usr/sbin/nft -f /etc/nftables/snitchwatch-endpoint-events.nft";
    assert_eq!(unit_values(&body, "Type"), ["oneshot"]);
    assert_eq!(unit_values(&body, "RemainAfterExit"), ["yes"]);
    assert_eq!(unit_values(&body, "ExecStart").join(" "), load);
    assert_eq!(unit_values(&body, "ExecReload").join(" "), load);
    assert_eq!(
        unit_values(&body, "ExecStop").join(" "),
        "/usr/sbin/nft delete table inet snitchwatch_endpoint_events"
    );
    // The file itself replaces the table in one transaction.
    let nft = read(NFT);
    let active: Vec<&str> = nft
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect();
    assert_eq!(
        &active[..2],
        [
            "table inet snitchwatch_endpoint_events",
            "delete table inet snitchwatch_endpoint_events"
        ]
    );
}

/// codex round 2, HIGH + MEDIUM: the listener is its own rsyslog instance,
/// which exists only while the table does and never starts on a config
/// error.
#[test]
fn listener_is_a_separate_rsyslog_instance_bound_to_the_table() {
    let body = read(LISTENER_UNIT);
    let nft_unit = "snitchwatch-endpoint-events-nft.service";
    for key in ["BindsTo", "After", "PartOf"] {
        assert!(
            unit_values(&body, key).iter().any(|unit| unit == nft_unit),
            "listener {key}= must name {nft_unit}"
        );
    }
    assert!(unit_values(&body, "Before").contains(&"opensnitch.service".to_string()));
    let conf = "/etc/snitchwatch-endpoint-events/rsyslog.conf";
    assert_eq!(
        unit_values(&body, "ExecStartPre").join(" "),
        format!("/usr/sbin/rsyslogd -N1 -f {conf}")
    );
    assert_eq!(
        unit_values(&body, "ExecStart").join(" "),
        format!("/usr/sbin/rsyslogd -n -iNONE -f {conf}")
    );
    assert_eq!(unit_values(&body, "UMask"), ["0077"]);

    // Its config is a whole main config of its own: no host inputs or
    // sinks, and it refuses to start unclean.
    let active = rsyslog_active();
    let global = statements(&active, "global").join(" ");
    assert_eq!(
        param(&global, "abortOnUncleanConfig").as_deref(),
        Some("on")
    );
    assert_eq!(param(&global, "workDirectory").as_deref(), Some(WORK_DIR));
    // codex round 3: under /var/lib/rsyslog, so it carries rsyslog's own
    // SELinux label (syslogd_var_lib_t), and the unit's StateDirectory is
    // the same directory.
    assert_eq!(
        unit_values(&body, "StateDirectory"),
        ["rsyslog/snitchwatch-endpoint-events"]
    );
    assert_eq!(unit_values(&body, "StateDirectoryMode"), ["0700"]);
    let modules: Vec<String> = statements(&active, "module")
        .iter()
        .filter_map(|body| param(body, "load"))
        .collect();
    assert_eq!(modules, ["imtcp", "mmjsonparse"]);
    assert!(!active.contains("include("), "no includes");

    // The host's rsyslog never carries the listener.
    let readme = read(README);
    assert!(!readme.contains("/etc/rsyslog.d/snitchwatch"));
}

/// The drop-in's uint32 pattern, applied by hand (no regex crate): each
/// alternative is a fixed-length digit template.
fn regex_lite_uint32() -> impl Fn(&str) -> bool {
    let active = rsyslog_active();
    let needle = "re_match($!Event!user_id, \"^(";
    let start = active.find(needle).unwrap() + needle.len();
    let end = start + active[start..].find(")\\$").unwrap();
    let alternatives: Vec<String> = active[start..end].split('|').map(str::to_string).collect();
    move |value: &str| alternatives.iter().any(|alt| template_matches(alt, value))
}

/// Whether `value` matches a template like `42[0-8][0-9]{7}` or `[0-9]{1,9}`
/// (literal digits, `[a-b]` ranges, `{n}` or `{m,n}` repeats).
fn template_matches(template: &str, value: &str) -> bool {
    let mut atoms: Vec<(char, char, usize, usize)> = Vec::new();
    let chars: Vec<char> = template.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let (lo, hi) = if chars[i] == '[' {
            let atom = (chars[i + 1], chars[i + 3]);
            i += 5;
            atom
        } else {
            i += 1;
            (chars[i - 1], chars[i - 1])
        };
        let (mut min, mut max) = (1, 1);
        if i < chars.len() && chars[i] == '{' {
            let close = i + chars[i..].iter().position(|c| *c == '}').unwrap();
            let spec: String = chars[i + 1..close].iter().collect();
            let (a, b) = spec.split_once(',').unwrap_or((&spec, &spec));
            (min, max) = (a.parse().unwrap(), b.parse().unwrap());
            i = close + 1;
        }
        atoms.push((lo, hi, min, max));
    }
    fn walk(atoms: &[(char, char, usize, usize)], value: &[char]) -> bool {
        let Some(&(lo, hi, min, max)) = atoms.first() else {
            return value.is_empty();
        };
        (min..=max.min(value.len())).any(|n| {
            value[..n].iter().all(|c| (lo..=hi).contains(c)) && walk(&atoms[1..], &value[n..])
        })
    }
    walk(&atoms, &value.chars().collect::<Vec<_>>())
}

/// codex round 2, MEDIUM: the daemon emits non-absolute paths for kernel
/// connections and unresolved executables (procmon/details.go:258-275); the
/// gate must keep those events. Patterns are checked as rsyslog would apply
/// them to these producer-derived values.
#[test]
fn path_gate_accepts_the_daemons_kernel_and_comm_values() {
    let active = rsyslog_active();
    let ruleset = ruleset_block(&active);
    assert!(ruleset.contains("$!Event!process_path == \"Kernel connection\""));
    // comm by its byte length only (codex round 3): a character class would
    // drop a comm holding a tab; jsonf escapes whatever it holds.
    // 15 bytes of comm, each possibly a byte Go's json.Marshal replaced with
    // U+FFFD (3 bytes) after the kernel cut a UTF-8 character (codex final).
    assert!(ruleset.contains("or strlen($!Event!process_path) <= 45)"));
    assert!(!ruleset.contains("[[:print:]]"));
    assert!(ruleset.contains("re_match($!Event!protocol, \"^(tcp|udp|udplite|sctp|icmp)6?\\$\")"));
    // uint32 by pattern: a RainerScript number literal can't hold 4294967295.
    assert!(ruleset.contains("|429496729[0-5])\\$\")"));
    assert!(!ruleset.contains("4294967295"));
    let uint32 = regex_lite_uint32();
    for (value, ok) in [
        ("0", true),
        ("1000", true),
        ("4294967295", true),
        ("4294967296", false),
        ("9999999999", false),
        ("42949672950", false),
    ] {
        assert_eq!(uint32(value), ok, "uid {value}");
    }
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
/// codex round 3: on a fresh install the README creates the work directory
/// (root, 0700) and restores SELinux labels BEFORE the first `-N1`, and its
/// install steps are marked so verify.sh runs them verbatim, in order.
#[test]
fn readme_creates_and_labels_the_work_dir_before_the_config_check() {
    let body = read(README);
    let marker = |name: &str| {
        body.find(&format!("<!-- verify.sh: {name} -->"))
            .unwrap_or_else(|| panic!("README has no `{name}` snippet"))
    };
    let (prepare, check, start) = (marker("prepare"), marker("check"), marker("start"));
    assert!(prepare < check && check < start, "snippets out of order");
    let prepare_block = &body[prepare..check];
    assert!(prepare_block.contains(&format!("install -d -m 0700 {WORK_DIR}")));
    let label = prepare_block.find("restorecon -RFv").expect("restorecon");
    assert!(prepare_block[label..].contains(WORK_DIR));
    assert!(prepare_block[label..].contains("/etc/pki/snitchwatch-endpoint-events"));
    assert!(body[check..start].contains("rsyslogd -N1 -f"));
}

/// codex final: the port check covers tcp ranges and a failed semanage, the
/// install records a port mapping it added, and the complete rollback
/// removes everything the recipe added (verify.sh runs both).
#[test]
fn readme_port_check_and_complete_rollback() {
    let body = read(README);
    let block = |name: &str| {
        let at = body
            .find(&format!("<!-- verify.sh: {name} -->"))
            .unwrap_or_else(|| panic!("README has no `{name}` snippet"));
        let start = at + body[at..].find("```bash").unwrap();
        let end = start + 7 + body[start + 7..].find("```").unwrap();
        body[start..end].to_string()
    };
    let marker = "/etc/snitchwatch-endpoint-events/selinux-port-added";
    let port = block("port");
    assert!(body.contains("policycoreutils-python-utils"));
    assert!(port.contains("$2 == \"tcp\""), "tcp rows only");
    assert!(port.contains("split($i, r, \"-\")"), "ranges");
    assert!(
        port.contains("semanage port -l") && port.contains("|| {"),
        "semanage must succeed"
    );
    assert!(port.contains("semanage port -a -t syslogd_port_t -p tcp"));
    assert!(port.contains(marker), "records the mapping it added");

    let rollback = block("rollback");
    for needle in [
        "disable --now snitchwatch-endpoint-events.service",
        "disable --now snitchwatch-endpoint-events-nft.service",
        "/etc/systemd/system/snitchwatch-endpoint-events.service",
        "/etc/systemd/system/snitchwatch-endpoint-events-nft.service",
        "/etc/nftables/snitchwatch-endpoint-events.nft",
        "/etc/snitchwatch-endpoint-events/rsyslog.conf",
        WORK_DIR,
        "/etc/pki/snitchwatch-endpoint-events",
        marker,
        "semanage port -d -p tcp",
        "daemon-reload",
    ] {
        assert!(rollback.contains(needle), "rollback misses `{needle}`");
    }
    assert!(body.find("<!-- verify.sh: way-back -->") < body.find("<!-- verify.sh: rollback -->"));
}

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
        "snitchwatch-endpoint-events.service",
        "snitchwatch-endpoint-events-nft.service",
        "reported by this host",
        "debug",
        "Server.Loggers",
        "restorecon -RFv",
        "semanage port -l)",
        "semanage port -a -t syslogd_port_t -p tcp",
        "6514",
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
    for (key, value) in json.as_object().unwrap() {
        let numeric = ["src_port", "dst_port", "user_id", "type"].contains(&key.as_str());
        assert_eq!(value.is_u64(), numeric, "`{key}` type in the sample");
        assert!(numeric || value.is_string(), "`{key}` is a string");
    }
    let body = read("packaging/endpoint-events/sample-event.json");
    for field in FORBIDDEN_FIELDS {
        assert!(!body.contains(field), "sample carries `{field}`");
    }
}

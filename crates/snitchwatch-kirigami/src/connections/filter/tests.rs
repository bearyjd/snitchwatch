use super::*;

fn row(id: &str, process: &str, host: &str, port: u16, action: Option<&str>) -> ConnectionRow {
    ConnectionRow {
        id: id.to_string(),
        process: process.to_string(),
        process_path: None,
        dst_host: host.to_string(),
        dst_ip: "10.0.0.1".to_string(),
        dst_port: port,
        protocol: "tcp".to_string(),
        direction: "outgoing".to_string(),
        action: action.map(|s| s.to_string()),
        bytes_sent: 0,
        bytes_received: 0,
        started_at_ms: 0,
        matched_rule: None,
        auto_answer: None,
        answer_deadline_ms: None,
        deferred: false,
        decided_by_default: false,
    }
}

#[test]
fn empty_filter_is_inactive_and_matches_all() {
    let f = ConnectionFilter::default();
    assert!(!f.is_active());
    assert!(f.matches(&row("a", "firefox", "github.com", 443, None)));
    assert!(f.matches(&row("b", "slack", "slack.com", 443, Some("allow"))));
}

#[test]
fn whitespace_only_query_is_inactive() {
    let f = ConnectionFilter::new("   ", false);
    assert!(!f.is_active());
}

#[test]
fn query_matches_process_host_and_is_case_insensitive() {
    let f = ConnectionFilter::new("GITHUB", false);
    assert!(f.is_active());
    assert!(f.matches(&row("a", "firefox", "github.com", 443, None)));
    assert!(!f.matches(&row("b", "slack", "slack.com", 443, None)));

    let f = ConnectionFilter::new("fire", false);
    assert!(f.matches(&row("a", "firefox", "github.com", 443, None)));
}

#[test]
fn query_matches_port_as_text() {
    let f = ConnectionFilter::new("8080", false);
    assert!(f.matches(&row("a", "proc", "h.example", 8080, None)));
    assert!(!f.matches(&row("b", "proc", "h.example", 443, None)));
}

#[test]
fn query_matches_verdict_token_including_pending() {
    assert!(ConnectionFilter::new("pending", false).matches(&row("a", "p", "h", 1, None)));
    assert!(ConnectionFilter::new("deny", false).matches(&row("a", "p", "h", 1, Some("deny"))));
    assert!(!ConnectionFilter::new("deny", false).matches(&row("a", "p", "h", 1, Some("allow"))));
}

#[test]
fn pending_only_excludes_decided_rows() {
    let f = ConnectionFilter::new("", true);
    assert!(f.is_active());
    assert!(f.matches(&row("a", "p", "h", 1, None)));
    assert!(!f.matches(&row("b", "p", "h", 1, Some("allow"))));
}

#[test]
fn pending_only_and_query_combine() {
    let f = ConnectionFilter::new("firefox", true);
    // pending + matches query
    assert!(f.matches(&row("a", "firefox", "h", 1, None)));
    // decided, excluded by pending_only even though query matches
    assert!(!f.matches(&row("b", "firefox", "h", 1, Some("allow"))));
    // pending but query misses
    assert!(!f.matches(&row("c", "chrome", "h", 1, None)));
}

use crate::geo::resolver::fakes::FakeLookup;
use std::sync::Arc;

fn ip_row(id: &str, process: &str, host: &str, ip: &str, port: u16, started: i64) -> ConnectionRow {
    let mut r = row(id, process, host, port, Some("allow"));
    r.dst_ip = ip.to_string();
    r.started_at_ms = started;
    r
}

fn with_geo(query: &str) -> ConnectionFilter {
    let lookup = FakeLookup::default().with("1.1.1.1", "DE", "Germany").with(
        "2.2.2.2",
        "US",
        "United States",
    );
    ConnectionFilter::new(query, false).with_geo(SharedResolver::new(Some(Arc::new(lookup))))
}

#[test]
fn app_token_is_a_case_insensitive_substring_of_process_or_path() {
    let f = ConnectionFilter::new("app:FIRE", false);
    assert!(f.matches(&row("a", "firefox", "slack.com", 443, None)));
    assert!(!f.matches(&row("b", "slack", "firefox.com", 443, None)));
    let mut r = row("c", "bin", "h", 1, None);
    r.process_path = Some("/usr/lib/Firefox/bin".to_string());
    assert!(f.matches(&r));
}

#[test]
fn host_token_is_a_case_insensitive_substring_of_host_or_ip() {
    let f = ConnectionFilter::new("host:GitHub", false);
    assert!(f.matches(&row("a", "p", "api.github.com", 443, None)));
    assert!(!f.matches(&row("b", "github", "slack.com", 443, None)));
    assert!(ConnectionFilter::new("host:10.0.", false).matches(&row("c", "p", "", 1, None)));
}

#[test]
fn port_token_is_exact() {
    let f = ConnectionFilter::new("port:443", false);
    assert!(f.matches(&row("a", "p", "h", 443, None)));
    assert!(!f.matches(&row("b", "p", "h", 4430, None)));
    assert!(!f.matches(&row("c", "p", "h", 44, None)));
}

#[test]
fn unparseable_port_matches_nothing_and_the_summary_says_so() {
    for q in ["port:abc", "port:99999", "port:-1"] {
        let f = ConnectionFilter::new(q, false);
        assert!(f.is_active(), "{q}");
        assert!(!f.matches(&row("a", "p", "h", 443, None)), "{q}");
        assert!(f.summary().contains("not a port number"), "{q}");
    }
}

#[test]
fn tokens_are_anded_in_any_order() {
    let f = ConnectionFilter::new("host:github port:443", false);
    assert!(f.matches(&row("a", "p", "github.com", 443, None)));
    assert!(!f.matches(&row("b", "p", "github.com", 80, None)));
    assert!(!f.matches(&row("c", "p", "slack.com", 443, None)));
    let f = ConnectionFilter::new("port:443 host:github", false);
    assert!(f.matches(&row("a", "p", "github.com", 443, None)));
    let f = ConnectionFilter::new("app:fire host:github deny", false);
    assert!(f.matches(&row("a", "firefox", "github.com", 1, Some("deny"))));
    assert!(!f.matches(&row("b", "firefox", "github.com", 1, Some("allow"))));
}

#[test]
fn prefixes_are_case_insensitive() {
    let f = ConnectionFilter::new("HOST:github PORT:443", false);
    assert!(f.matches(&row("a", "p", "github.com", 443, None)));
}

#[test]
fn blank_whitespace_and_empty_value_tokens_are_ignored() {
    for q in [
        "",
        "   ",
        "\t \n",
        "host:",
        "app:",
        "port:",
        "country:",
        "host: port:  ",
    ] {
        let f = ConnectionFilter::new(q, false);
        assert!(!f.is_active(), "{q:?} should be inactive");
        assert!(f.matches(&row("a", "p", "h", 1, None)), "{q:?}");
    }
    // An empty token beside a real one leaves the real one in force.
    let f = ConnectionFilter::new("  host:  app:fire ", false);
    assert!(f.matches(&row("a", "firefox", "h", 1, None)));
    assert!(!f.matches(&row("b", "slack", "h", 1, None)));
}

#[test]
fn unknown_prefixes_are_free_text() {
    // "ip:" is not a token kind, so the whole token is a free-text needle
    // that appears in no field.
    let f = ConnectionFilter::new("ip:10.0.0.1", false);
    assert!(f.is_active());
    assert!(!f.matches(&row("a", "p", "h", 1, None)));
    let mut r = row("b", "weird:name", "h", 1, None);
    r.dst_ip = "x".to_string();
    assert!(ConnectionFilter::new("weird:name", false).matches(&r));
    assert!(ConnectionFilter::new("::1", false).is_active());
}

#[test]
fn free_text_still_searches_verdict_and_port_text() {
    assert!(ConnectionFilter::new("pending", false).matches(&row("a", "p", "h", 1, None)));
    assert!(ConnectionFilter::new("844", false).matches(&row("a", "p", "h", 8443, None)));
}

#[test]
fn country_matches_the_resolved_country_case_insensitively() {
    let f = with_geo("country:de");
    assert!(f.matches(&ip_row("a", "p", "h", "1.1.1.1", 1, 0)));
    assert!(!f.matches(&ip_row("b", "p", "h", "2.2.2.2", 1, 0)));
    // Unresolvable and local addresses never match a country.
    assert!(!f.matches(&ip_row("c", "p", "h", "9.9.9.9", 1, 0)));
    assert!(!f.matches(&ip_row("d", "p", "h", "10.0.0.1", 1, 0)));
    assert!(with_geo("COUNTRY:US").matches(&ip_row("b", "p", "h", "2.2.2.2", 1, 0)));
    assert_eq!(f.summary(), "search \"country:de\"");
}

#[test]
fn country_combines_with_other_tokens() {
    let f = with_geo("country:DE host:example");
    assert!(f.matches(&ip_row("a", "p", "example.org", "1.1.1.1", 1, 0)));
    assert!(!f.matches(&ip_row("b", "p", "other.org", "1.1.1.1", 1, 0)));
}

#[test]
fn country_matches_nothing_without_a_database_and_says_so() {
    for geo in [None, Some(SharedResolver::new(None))] {
        let mut f = ConnectionFilter::new("country:DE", false);
        if let Some(g) = geo {
            f = f.with_geo(g);
        }
        assert!(f.is_active());
        assert!(!f.matches(&ip_row("a", "p", "h", "1.1.1.1", 1, 0)));
        assert!(f.summary().contains("no GeoIP database is installed"));
    }
}

#[test]
fn since_boundary_keeps_rows_at_or_after_the_start() {
    let mut f = ConnectionFilter::default();
    f.set_since_ms(Some(1_000));
    assert!(f.is_active());
    assert!(!f.matches(&ip_row("a", "p", "h", "1.1.1.1", 1, 999)));
    assert!(f.matches(&ip_row("b", "p", "h", "1.1.1.1", 1, 1_000)));
    assert!(f.matches(&ip_row("c", "p", "h", "1.1.1.1", 1, 1_001)));
    assert_eq!(f.since_ms(), Some(1_000));
    f.set_since_ms(None);
    assert!(!f.is_active());
    assert!(f.matches(&ip_row("a", "p", "h", "1.1.1.1", 1, 999)));
}

#[test]
fn denied_only_keeps_denied_rows_including_default_denials() {
    let mut f = ConnectionFilter::default();
    f.set_denied_only(true);
    assert!(f.is_active());
    assert!(f.matches(&row("a", "p", "h", 1, Some("deny"))));
    assert!(!f.matches(&row("b", "p", "h", 1, Some("allow"))));
    assert!(!f.matches(&row("c", "p", "h", 1, None)));
    let mut d = row("d", "p", "h", 1, Some("deny"));
    d.decided_by_default = true;
    assert!(f.matches(&d));
}

#[test]
fn denied_only_and_pending_only_together_match_nothing() {
    // A row cannot be both undecided and denied.
    let mut f = ConnectionFilter::default();
    f.set_denied_only(true);
    f.set_pending_only(true);
    for a in [None, Some("deny"), Some("allow")] {
        assert!(!f.matches(&row("a", "p", "h", 1, a)), "{a:?}");
    }
    f.set_pending_only(false);
    assert!(f.matches(&row("a", "p", "h", 1, Some("deny"))));
}

#[test]
fn every_option_combines_and_clearing_each_restores_the_list() {
    let mut f = with_geo("country:DE app:fire");
    f.set_denied_only(true);
    f.set_since_ms(Some(100));
    let mut hit = ip_row("a", "firefox", "h", "1.1.1.1", 1, 100);
    hit.action = Some("deny".to_string());
    assert!(f.matches(&hit));
    let mut old = hit.clone();
    old.started_at_ms = 99;
    assert!(!f.matches(&old));
    f.set_since_ms(None);
    f.set_denied_only(false);
    f.set_query("");
    assert!(!f.is_active());
}

#[test]
fn equality_ignores_the_resolver_but_not_the_options() {
    assert_eq!(with_geo("host:a"), ConnectionFilter::new("host:a", false));
    let mut f = ConnectionFilter::default();
    f.set_denied_only(true);
    assert_ne!(f, ConnectionFilter::default());
    let mut g = ConnectionFilter::default();
    g.set_since_ms(Some(1));
    assert_ne!(g, ConnectionFilter::default());
}

#[test]
fn summary_is_empty_when_inactive_and_lists_active_options() {
    assert_eq!(ConnectionFilter::default().summary(), "");
    let mut f = ConnectionFilter::new("host:a", true);
    f.set_denied_only(true);
    f.set_since_ms(Some(5));
    assert_eq!(
        f.summary(),
        "search \"host:a\", pending only, denied only, \
         from now (hides rows that started earlier, even if still active). \
         Pending only and denied only cannot both match, so no rows match."
    );
    let mut both = ConnectionFilter::default();
    both.set_pending_only(true);
    both.set_denied_only(true);
    assert!(both.summary().contains("cannot both match"));
    let mut one = ConnectionFilter::default();
    one.set_pending_only(true);
    assert!(!one.summary().contains("cannot both match"));
}

#[test]
fn repeated_tokens_are_anded() {
    let f = ConnectionFilter::new("host:a host:b", false);
    assert!(f.matches(&row("x", "p", "a.b.com", 1, None)));
    assert!(!f.matches(&row("y", "p", "a.com", 1, None)));
    assert!(!f.matches(&row("z", "p", "b.com", 1, None)));
}

#[test]
fn quoted_phrases_are_one_free_text_term() {
    assert_eq!(
        parse_terms("\"foo bar\" baz"),
        vec![Term::Free("foo bar".into()), Term::Free("baz".into())]
    );
    // Unterminated: the rest of the string is the phrase.
    assert_eq!(
        parse_terms("a \"foo bar baz"),
        vec![Term::Free("a".into()), Term::Free("foo bar baz".into())]
    );
    // A phrase is free text even with a colon in it; empty phrases vanish.
    assert_eq!(
        parse_terms("\"host:x\" \"\""),
        vec![Term::Free("host:x".into())]
    );
    // A quoted value after a prefix keeps its spaces.
    assert_eq!(
        parse_terms("app:\"my app\""),
        vec![Term::App("my app".into())]
    );
    let f = ConnectionFilter::new("\"foo bar\"", false);
    assert!(f.matches(&row("a", "foo bar", "h", 1, None)));
    assert!(!f.matches(&row("b", "foo", "bar", 1, None)));
}

#[test]
fn auto_select_picks_first_new_pending_when_nothing_selected() {
    assert_eq!(
        auto_select_target(None, false, &["p1", "p2"]),
        Some("p1".to_string())
    );
}

#[test]
fn auto_select_moves_off_a_decided_selection() {
    // Current selection is a decided row -> ok to surface the new pending one.
    assert_eq!(
        auto_select_target(Some("old"), false, &["p1"]),
        Some("p1".to_string())
    );
}

#[test]
fn auto_select_does_not_steal_focus_from_a_pending_row() {
    // User is investigating a pending row -> leave selection alone.
    assert_eq!(auto_select_target(Some("busy"), true, &["p1"]), None);
}

#[test]
fn auto_select_none_when_no_new_pending() {
    assert_eq!(auto_select_target(None, false, &[]), None);
    assert_eq!(auto_select_target(Some("x"), false, &[]), None);
}
#[test]
fn geographic_scope_preserves_query_and_matches_canonical_ip_exactly() {
    let mut f = ConnectionFilter::new("app:fire", false);
    f.set_geo_scope(GeoScope::parse("ip", "ip:2001:4860:4860::8888"));
    let matching = ip_row("a", "firefox", "dns", "2001:4860:4860:0:0:0:0:8888", 443, 0);
    assert!(f.matches(&matching));
    assert!(!f.matches(&ip_row(
        "b",
        "firefox",
        "dns",
        "2001:4860:4860::8889",
        443,
        0
    )));
    assert!(!f.matches(&ip_row("c", "other", "dns", "2001:4860:4860::8888", 443, 0)));
    f.set_geo_scope(GeoScope::parse("ip", "ip:1.1.1.1"));
    assert!(!f.matches(&ip_row("d", "firefox", "dns", "11.1.1.1", 443, 0)));
    assert_eq!(f.query(), "app:fire");
    f.set_geo_scope(None);
    assert!(f.matches(&matching));
    assert_eq!(f.query(), "app:fire");
}

#[test]
fn geographic_scope_country_and_unavailable_city_use_shared_bucket_identity() {
    let mut f = with_geo("app:fire");
    f.set_geo_scope(GeoScope::parse("country", "country:DE"));
    assert!(f.matches(&ip_row("a", "firefox", "dns", "1.1.1.1", 443, 0)));
    assert!(!f.matches(&ip_row("b", "firefox", "dns", "2.2.2.2", 443, 0)));
    f.set_geo_scope(GeoScope::parse("city", "country:DE:unknown-city"));
    assert!(f.matches(&ip_row("a", "firefox", "dns", "1.1.1.1", 443, 0)));
    f.set_geo_scope(GeoScope::parse("country", "local"));
    assert!(f.matches(&ip_row("c", "firefox", "router", "192.168.1.1", 443, 0)));
    f.set_geo_scope(GeoScope::parse("country", "unknown"));
    assert!(f.matches(&ip_row("d", "firefox", "dns", "9.9.9.9", 443, 0)));
    assert!(!f.matches(&ip_row("c", "firefox", "router", "192.168.1.1", 443, 0)));
}

#[test]
fn geographic_scope_city_uses_shared_composite_key_including_subdivision() {
    use crate::geo::resolver::{CountryInfo, CountryLookup, GeoCoordinates, GeoPlace};
    struct Cities;
    impl CountryLookup for Cities {
        fn lookup(&self, _ip: std::net::IpAddr) -> Option<CountryInfo> {
            None
        }
        fn lookup_place(&self, ip: std::net::IpAddr) -> Option<GeoPlace> {
            Some(GeoPlace {
                country: CountryInfo {
                    code: "US".into(),
                    name: "United States".into(),
                },
                subdivisions: vec![if ip.to_string() == "1.1.1.1" {
                    "North"
                } else {
                    "South"
                }
                .into()],
                city: Some("Same city".into()),
                coordinates: GeoCoordinates::new(Some(1.0), Some(2.0)),
                provider: "fixture".into(),
                precision: None,
            })
        }
    }
    let geo = SharedResolver::new(Some(Arc::new(Cities)));
    let key = geo.resolve("1.1.1.1").place.unwrap().city_key();
    let mut f = ConnectionFilter::default().with_geo(geo);
    f.set_geo_scope(GeoScope::parse("city", &key));
    assert!(f.is_active());
    assert!(f.matches(&ip_row("a", "firefox", "h", "1.1.1.1", 443, 0)));
    assert!(!f.matches(&ip_row("b", "firefox", "h", "2.2.2.2", 443, 0)));
    let old = f.clone();
    f.set_geo_scope(GeoScope::parse("city", "city:removed"));
    assert_ne!(f, old, "scope participates in projection identity");
    assert!(
        !f.matches(&ip_row("a", "firefox", "h", "1.1.1.1", 443, 0)),
        "vanished city stays empty"
    );
}

#[test]
fn geographic_scope_rejects_invalid_inputs() {
    for (kind, key) in [
        ("ip", "ip:1.1.1"),
        ("ip", "1.1.1.1"),
        ("country", "country:DEU"),
        ("country", "country:de"),
        ("city", ""),
        ("city", "country:DE"),
        ("wat", "country:DE"),
    ] {
        assert!(GeoScope::parse(kind, key).is_none(), "{kind}: {key}");
    }
}

#[test]
fn prepared_geographic_scope_never_decodes_after_shared_cache_overflow() {
    use crate::geo::resolver::{CountryInfo, CountryLookup};
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Counting(Arc<AtomicUsize>);
    impl CountryLookup for Counting {
        fn lookup(&self, _ip: std::net::IpAddr) -> Option<CountryInfo> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Some(CountryInfo {
                code: "DE".into(),
                name: "Germany".into(),
            })
        }
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let geo = SharedResolver::new(Some(Arc::new(Counting(calls.clone()))));
    let r = ip_row("prepared", "browser", "dns", "1.1.1.1", 443, 0);
    let mut facts = std::collections::HashMap::new();
    facts.insert(r.id.clone(), geo.resolve(&r.dst_ip));
    for i in 0..65_538u32 {
        geo.resolve(&std::net::Ipv4Addr::from(0x0b00_0000 + i).to_string());
    }
    let before = calls.load(Ordering::SeqCst);
    let mut f = ConnectionFilter::default().with_geo(geo).prepared_only();
    f.merge_prepared_geo(facts);
    for scope in [
        ("country", "country:DE"),
        ("city", "country:DE:unknown-city"),
        ("ip", "ip:1.1.1.1"),
    ] {
        f.set_geo_scope(GeoScope::parse(scope.0, scope.1));
        assert!(f.matches(&r));
    }
    let mut untracked = r.clone();
    untracked.id = "outside-geo-capacity".into();
    assert!(
        !f.matches(&untracked),
        "missing prepared facts must not masquerade as Unknown or broaden scope"
    );
    f.set_query("country:DE");
    f.set_geo_scope(None);
    assert!(f.matches(&r));
    assert_eq!(
        calls.load(Ordering::SeqCst),
        before,
        "scope evaluation decoded MMDB on Qt-equivalent path"
    );
}

fn recommended_row(id: &str) -> ConnectionRow {
    ConnectionRow {
        process_path: Some("/usr/sbin/chronyd".to_string()),
        matched_rule: Some("snitchwatch-default-chronyd".to_string()),
        ..row(id, "chronyd", "pool.ntp.org", 123, Some("allow"))
    }
}

fn attention_facts() -> Arc<AttentionFacts> {
    let rule = crate::rules::row_store::Rule {
        name: "snitchwatch-default-chronyd".to_string(),
        enabled: true,
        ..Default::default()
    };
    Arc::new(AttentionFacts::new(&[rule], ["/usr/sbin/chronyd"]))
}

#[test]
fn needs_attention_collapses_handled_rows_only_while_on() {
    let mut f = ConnectionFilter::default();
    f.set_attention_facts(attention_facts());
    assert!(!f.is_active(), "off by default in the bare filter");
    assert!(f.matches(&recommended_row("a")));
    assert!(f.is_handled(&recommended_row("a")));

    f.set_needs_attention(true);
    assert!(f.needs_attention());
    assert!(f.is_active());
    assert!(!f.matches(&recommended_row("a")));
    let pending = ConnectionRow {
        action: None,
        ..recommended_row("p")
    };
    assert!(f.matches(&pending), "a pending row always shows");
    assert!(f.matches(&row("b", "firefox", "github.com", 443, Some("allow"))));
}

#[test]
fn needs_attention_fails_open_without_recommended_rules() {
    let mut f = ConnectionFilter::default();
    f.set_needs_attention(true);
    assert!(!f.is_active(), "no facts: nothing collapses");
    assert!(f.matches(&recommended_row("a")));
    f.set_attention_facts(Arc::new(AttentionFacts::new(&[], ["/usr/sbin/chronyd"])));
    assert!(!f.is_active(), "an empty rules list collapses nothing");
    assert!(f.matches(&recommended_row("a")));
    assert!(!f.is_handled(&recommended_row("a")));
    assert_eq!(f.summary(), "", "the default adds no filter text");
}

#[test]
fn needs_attention_is_part_of_the_filter_identity_but_the_facts_are_not() {
    let mut a = ConnectionFilter::default();
    let b = ConnectionFilter::default();
    a.set_attention_facts(attention_facts());
    assert_eq!(a, b);
    a.set_needs_attention(true);
    assert_ne!(a, b);
}

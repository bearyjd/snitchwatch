use serde_json::Value;

use super::*;

fn rows(s: &ReviewSession) -> Vec<Value> {
    serde_json::from_str::<Vec<Value>>(&s.rows_json()).unwrap()
}

#[test]
fn rows_carry_the_fields_the_program_label_reads_and_the_facts() {
    let s = session(vec![narrow("alpha"), tmp("gamma")]);
    let rows = rows(&s);
    assert_eq!(rows.len(), 2);
    for row in &rows {
        for field in [
            "programLabel",
            "programLabelUnverified",
            "programPathTail",
            "programPathFull",
            "flags",
            "top",
            "choices",
        ] {
            assert!(row.get(field).is_some(), "missing {field}");
        }
    }
    let gamma = rows
        .iter()
        .find(|r| r["programPathFull"] == "/tmp/gamma")
        .unwrap();
    assert!(!gamma["flags"].as_array().unwrap().is_empty(), "{gamma}");
    let kinds: Vec<&str> = gamma["choices"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["kind"].as_str().unwrap())
        .collect();
    assert!(
        kinds.contains(&"block") && kinds.contains(&"allow-any"),
        "{kinds:?}"
    );
}

#[test]
fn a_row_with_no_path_says_why_and_offers_nothing() {
    let s = session(vec![program("daemon", None, &[("a.example", 3)])]);
    let rows = rows(&s);
    assert!(rows[0]["choices"].as_array().unwrap().is_empty());
    assert!(rows[0]["notSelectable"]
        .as_str()
        .unwrap()
        .contains("cannot make a rule"));
}

#[test]
fn hosts_of_the_narrow_choice_are_listed_inline() {
    let s = session(vec![narrow("alpha")]);
    let row = &rows(&s)[0];
    let hosts = row["choices"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["kind"] == "allow-hosts")
        .unwrap()["hosts"]
        .clone();
    assert_eq!(hosts, serde_json::json!(["b.example", "a.example"]));
    assert_eq!(row["suggested"], true);
}

#[test]
fn ticks_are_a_separate_small_document() {
    let s = session(vec![narrow("alpha")]);
    let key = key_of(&s, "/usr/bin/alpha");
    let ticked = s.tick(&key, Some(K::Block)).unwrap();
    let ticks: Value = serde_json::from_str(&ticked.ticks_json()).unwrap();
    assert_eq!(ticks[&key], "block");
    assert_eq!(
        ticked.rows_json(),
        s.rows_json(),
        "a tick leaves the rows alone"
    );
}

#[test]
fn hostile_text_stays_text_in_the_json() {
    let name = "<b>x</b>\"}\u{202e}";
    let s = session(vec![program(
        name,
        Some("/usr/bin/<img src=x>"),
        &[("a.example", 2)],
    )]);
    let text = s.rows_json();
    let parsed: Vec<Value> = serde_json::from_str(&text).unwrap();
    assert_eq!(parsed.len(), 1);
    assert!(!text.contains('\u{202e}'), "bidi override kept");
}

#[test]
fn notices_say_what_the_record_lacks() {
    let mut aggregate = Aggregate::default();
    aggregate.absorb(narrow("alpha"));
    aggregate.incomplete = true;
    let s = ReviewSession::new(&aggregate, &[], false);
    let n: Value = serde_json::from_str(&s.notices_json()).unwrap();
    assert_eq!(n["programs"], 1);
    assert_eq!(n["incomplete"], true);
    assert_eq!(n["rulesRead"], false);
    assert_eq!(n["classAFull"], false);
}

#[test]
fn a_full_directory_entry_and_its_counted_only_programs_are_reported() {
    let mut aggregate = Aggregate::default();
    for i in 0..(crate::learned::MAX_DIR_PROGRAMS + 3) {
        aggregate.absorb(program(
            &format!("p{i}"),
            Some(&format!("/tmp/junk/p{i}")),
            &[("a.example", 2)],
        ));
    }
    let s = ReviewSession::new(&aggregate, &[], true);
    let n: Value = serde_json::from_str(&s.notices_json()).unwrap();
    assert_eq!(n["dirsFull"], 1);
    assert!(n["dirUnlistedConnections"].as_u64().unwrap() > 0, "{n}");
    assert_eq!(rows(&s).len(), crate::learned::MAX_DIR_PROGRAMS);
}

/// Every string anywhere in `value`.
fn strings(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(s) => out.push(s.clone()),
        Value::Array(a) => a.iter().for_each(|v| strings(v, out)),
        // `key` is the recorder's raw key: an id the page hands back, never
        // text it shows.
        Value::Object(o) => o
            .iter()
            .filter(|(name, _)| name.as_str() != "key")
            .for_each(|(_, v)| strings(v, out)),
        _ => {}
    }
}

#[test]
fn no_text_shown_carries_a_control_or_bidi_character() {
    let evil = "x\n\r\t\u{7}\u{202e}\u{2066}y";
    let s = session(vec![
        // Hostile name, path, hosts (also as the narrow choice's hosts).
        program(
            &format!("n{evil}"),
            Some(&format!("/usr/bin/p{evil}")),
            &[(&format!("h{evil}.example"), 3), ("ok.example", 2)],
        ),
        // A clean path, so its host list is offered.
        program(
            "clean",
            Some("/usr/bin/clean"),
            &[(&format!("b{evil}.example"), 3), ("ok.example", 2)],
        ),
        program(
            &format!("m{evil}"),
            Some(&format!("/tmp/q{evil}")),
            &[(&format!("g{evil}.example"), 2)],
        ),
    ]);
    let mut all = Vec::new();
    strings(
        &serde_json::from_str::<Value>(&s.rows_json()).unwrap(),
        &mut all,
    );
    assert!(!all.is_empty());
    for text in &all {
        assert!(
            !text.chars().any(|c| c.is_control()
                || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')),
            "{text:?}"
        );
    }
    // The rule lines of the summary too.
    let ticked = s.select_suggested();
    if let Ok(frozen) = ticked.freeze() {
        let mut lines = Vec::new();
        strings(
            &serde_json::from_str::<Value>(&frozen.summary_json()).unwrap(),
            &mut lines,
        );
        for text in &lines {
            assert!(!text.chars().any(|c| c.is_control()), "{text:?}");
        }
    }
}

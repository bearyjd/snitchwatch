use super::*;

#[test]
fn the_plan_is_exactly_the_ticked_choices_and_nothing_else() {
    let s = session(vec![narrow("alpha"), narrow("beta"), tmp("gamma")]);
    let (alpha, beta) = (key_of(&s, "/usr/bin/alpha"), key_of(&s, "/usr/bin/beta"));
    let gamma = key_of(&s, "/tmp/gamma");
    let frozen = s
        .tick(&alpha, Some(K::AllowHosts))
        .unwrap()
        .tick(&gamma, Some(K::Block))
        .unwrap()
        .freeze()
        .unwrap();
    let plan = frozen.authorize().unwrap();
    let names: Vec<&str> = plan.rules().iter().map(|r| r.name()).collect();
    // alpha: two host rules; gamma: one block; beta (not ticked): none.
    assert_eq!(names.len(), 3, "{names:?}");
    assert_eq!(
        names
            .iter()
            .filter(|n| n.starts_with(crate::learning_review::BLOCK_NAME_PREFIX))
            .count(),
        1
    );
    let beta_paths = crate::learning_review::rules_for(
        &crate::learning_review::Choice::AllowAny,
        frozen.program(&beta).unwrap(),
    )
    .unwrap();
    assert!(!names.contains(&beta_paths[0].name()));
    let expected_alpha = crate::learning_review::rules_for(
        &frozen
            .proposals()
            .iter()
            .find(|p| p.key == alpha)
            .unwrap()
            .choices[0],
        frozen.program(&alpha).unwrap(),
    )
    .unwrap();
    for payload in expected_alpha {
        assert!(names.contains(&payload.name()), "{}", payload.name());
    }
}

#[test]
fn the_summary_repeats_every_rule_in_plain_words_one_line_each() {
    let s = session(vec![narrow("alpha"), tmp("gamma")]);
    let (alpha, gamma) = (key_of(&s, "/usr/bin/alpha"), key_of(&s, "/tmp/gamma"));
    let frozen = s
        .tick(&alpha, Some(K::AllowHosts))
        .unwrap()
        .tick(&gamma, Some(K::AllowAny))
        .unwrap()
        .freeze()
        .unwrap();
    let summary = frozen.summary().unwrap();
    assert_eq!(
        summary.lines.len(),
        frozen.authorize().unwrap().rules().len()
    );
    assert_eq!(summary.lines.len(), 3);
    let all = summary.lines.join("\n");
    assert!(
        all.contains("b.example") && all.contains("a.example"),
        "{all}"
    );
    assert!(all.contains("ANY destination"), "{all}");
    assert!(
        all.contains("/usr/bin/alpha") && all.contains("/tmp/gamma"),
        "{all}"
    );
    assert!(frozen.summary_json().contains("\"count\":3"));
}

#[test]
fn nothing_ticked_makes_no_plan() {
    let s = session(vec![narrow("alpha")]);
    assert_eq!(s.freeze().unwrap_err(), Refusal::NothingTicked);
    assert_eq!(s.authorize().unwrap_err(), Refusal::NotFrozen);
}

#[test]
fn blocking_a_system_path_needs_its_own_confirmation_before_apply() {
    let s = session(vec![narrow("alpha"), tmp("gamma")]);
    let (alpha, gamma) = (key_of(&s, "/usr/bin/alpha"), key_of(&s, "/tmp/gamma"));
    // A block of a user-writable path needs none.
    let plain = s.tick(&gamma, Some(K::Block)).unwrap().freeze().unwrap();
    assert!(plain.summary().unwrap().extras.is_empty());
    assert!(plain.authorize().is_ok());

    let frozen = s.tick(&alpha, Some(K::Block)).unwrap().freeze().unwrap();
    let summary = frozen.summary().unwrap();
    assert_eq!(summary.extras.len(), 1);
    assert!(summary.extras[0].contains(crate::learning_review::BLOCK_WARNING));
    assert!(summary.extras[0].contains("/usr/bin/alpha"));
    assert_eq!(frozen.authorize().unwrap_err(), Refusal::Unconfirmed(1));
    let confirmed = frozen.confirm(0, true);
    assert_eq!(confirmed.authorize().unwrap().rules().len(), 1);
    // Taking it back refuses again; an index out of range changes nothing.
    assert!(confirmed.confirm(0, false).authorize().is_err());
    assert!(confirmed.confirm(9, false).authorize().is_ok());
}

#[test]
fn any_destination_for_a_dual_use_program_needs_a_confirmation() {
    let s = session(vec![dual("python3")]);
    let key = key_of(&s, "/usr/bin/python3");
    let frozen = s.tick(&key, Some(K::AllowAny)).unwrap().freeze().unwrap();
    let extras = frozen.summary().unwrap().extras;
    assert_eq!(extras.len(), 1);
    assert!(extras[0].contains(crate::learning_review::DUAL_USE_WARNING));
    assert_eq!(frozen.authorize().unwrap_err(), Refusal::Unconfirmed(1));
    // The narrow choice of the same program needs none.
    let narrow = s.tick(&key, Some(K::AllowHosts));
    if let Ok(narrow) = narrow {
        assert!(narrow.freeze().unwrap().authorize().is_ok());
    }
}

#[test]
fn every_extra_must_be_given() {
    let s = session(vec![narrow("alpha"), dual("python3")]);
    let (a, p) = (key_of(&s, "/usr/bin/alpha"), key_of(&s, "/usr/bin/python3"));
    let frozen = s
        .tick(&a, Some(K::Block))
        .unwrap()
        .tick(&p, Some(K::AllowAny))
        .unwrap()
        .freeze()
        .unwrap();
    assert_eq!(frozen.summary().unwrap().extras.len(), 2);
    let one = frozen.confirm(0, true);
    assert_eq!(one.authorize().unwrap_err(), Refusal::Unconfirmed(1));
    assert!(one.confirm(1, true).authorize().is_ok());
}

#[test]
fn a_system_service_name_outside_a_system_path_still_needs_the_block_confirmation() {
    let s = session(vec![program(
        "NetworkManager",
        Some("/opt/x/NetworkManager"),
        &[("a.example", 2)],
    )]);
    let key = key_of(&s, "/opt/x/NetworkManager");
    let frozen = s.tick(&key, Some(K::Block)).unwrap().freeze().unwrap();
    assert_eq!(frozen.summary().unwrap().extras.len(), 1);
    assert!(frozen.authorize().is_err());
}

#[test]
fn a_row_without_a_usable_path_offers_no_block_so_no_unconfirmed_block_exists() {
    let s = session(vec![program("NetworkManager", None, &[("a.example", 2)])]);
    let key = s.proposals()[0].key.clone();
    assert_eq!(
        s.tick(&key, Some(K::Block)).unwrap_err(),
        Refusal::NotOffered
    );
}

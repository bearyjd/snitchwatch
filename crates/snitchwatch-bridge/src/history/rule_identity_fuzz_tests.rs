//! PR 4a: table-driven collision check for `rule_identity`. Over many random
//! rules, canonical bytes (and hashes) are equal exactly when an
//! independently built visible form is equal: every kept field distinguishes,
//! only withheld values and dropped fields do not.

use snitchwatch_proto::protocol::{Operator, Rule};

use super::rule_identity::identify;

#[derive(Debug, Clone, PartialEq)]
enum Val {
    Kept(String),
    Withheld,
    None,
}

#[derive(Debug, Clone, PartialEq)]
struct Node {
    kind: String,
    operand: String,
    sensitive: bool,
    value: Val,
    members: Vec<Node>,
}

#[derive(Debug, Clone, PartialEq)]
struct Visible {
    name: String,
    action: String,
    duration: String,
    flags: (bool, bool, bool),
    tree: Option<Node>,
}

struct Lcg(u64);
impl Lcg {
    fn pick<'a>(&mut self, set: &[&'a str]) -> &'a str {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        set[((self.0 >> 33) as usize) % set.len()]
    }
    fn flag(&mut self) -> bool {
        self.pick(&["0", "1"]) == "1"
    }
}

const WORDS: [&str; 6] = ["", "a", "ab", "\0\0\0\x01", "x.y", "/p"];
const TYPES: [&str; 6] = ["simple", "regexp", "list", "lists", "network", "bogus"];
const OPERANDS: [&str; 11] = [
    "dest.host",
    "process.path",
    "process.command",
    "process.env.A",
    "process.env.B",
    "lists.ips",
    "true",
    "list",
    "weird",
    "weirder",
    "",
];
const KEPT: [&str; 3] = ["dest.host", "process.path", "lists.ips"];
const ACTIONS: [&str; 4] = ["allow", "deny", "reject", "bogus!"];
const DURATIONS: [&str; 5] = ["once", "1.5h", "2.5h", "always", "bad!"];

fn gen_op(rng: &mut Lcg, depth: usize) -> (Operator, Node) {
    let kind = rng.pick(&TYPES);
    let operand = rng.pick(&OPERANDS);
    let data = rng.pick(&WORDS);
    let sensitive = rng.flag();
    let mut members = Vec::new();
    let mut nodes = Vec::new();
    if depth < 2 {
        for _ in 0..usize::from(rng.flag()) + usize::from(rng.flag()) {
            let (m, n) = gen_op(rng, depth + 1);
            members.push(m);
            nodes.push(n);
        }
    }
    let value = if kind == "list" || operand == "true" || operand == "list" {
        Val::None
    } else if KEPT.contains(&operand) {
        Val::Kept(data.to_owned())
    } else {
        Val::Withheld
    };
    let wire = Operator {
        r#type: kind.into(),
        operand: operand.into(),
        data: data.into(),
        sensitive,
        list: members,
    };
    let node = Node {
        kind: if TYPES[..5].contains(&kind) {
            kind
        } else {
            "?"
        }
        .into(),
        operand: operand.into(),
        sensitive,
        value,
        members: nodes,
    };
    (wire, node)
}

fn gen_rule(rng: &mut Lcg) -> (Rule, Visible) {
    let name = rng.pick(&WORDS);
    let action = rng.pick(&ACTIONS);
    let duration = rng.pick(&DURATIONS);
    let (enabled, precedence, nolog) = (rng.flag(), rng.flag(), rng.flag());
    let (wire, tree) = if rng.pick(&["a", "b", "c", "d", "e"]) == "a" {
        (None, None)
    } else {
        let (o, n) = gen_op(rng, 0);
        (Some(o), Some(n))
    };
    let rule = Rule {
        name: name.into(),
        action: action.into(),
        duration: duration.into(),
        enabled,
        precedence,
        nolog,
        description: rng.pick(&WORDS).into(),
        created: i64::from(rng.flag()),
        operator: wire,
    };
    let visible = Visible {
        name: name.into(),
        action: if action == "bogus!" { "?" } else { action }.into(),
        duration: if duration == "bad!" { "?" } else { duration }.into(),
        flags: (enabled, precedence, nolog),
        tree,
    };
    (rule, visible)
}

#[test]
fn canonical_bytes_and_hashes_are_equal_exactly_when_the_visible_forms_are() {
    let mut rng = Lcg(7);
    let mut seen = Vec::new();
    for _ in 0..600 {
        let (rule, visible) = gen_rule(&mut rng);
        let ident = identify(&rule).expect("identity");
        seen.push((ident.canonical, ident.hash, visible));
    }
    let mut equal_pairs = 0;
    for (i, (ca, ha, va)) in seen.iter().enumerate() {
        for (cb, hb, vb) in &seen[i + 1..] {
            assert_eq!(ca == cb, va == vb, "{va:?} vs {vb:?}");
            assert_eq!(ha == hb, va == vb, "hash: {va:?} vs {vb:?}");
            equal_pairs += usize::from(va == vb);
        }
    }
    assert!(equal_pairs > 0, "the table exercises the equal case too");
}

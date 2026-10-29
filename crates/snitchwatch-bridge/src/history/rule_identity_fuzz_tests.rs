//! PR 4a: table-driven collision check for `rule_identity`: over random
//! rules, canonical bytes are equal exactly when an independently built
//! visible form is equal.

use snitchwatch_proto::protocol::{Operator, Rule};

use super::rule_identity::identify;

fn op(kind: &str, operand: &str, data: &str) -> Operator {
    Operator {
        r#type: kind.into(),
        operand: operand.into(),
        data: data.into(),
        sensitive: false,
        list: vec![],
    }
}

// ---- fuzz-style: canonical equality <=> visible-form equality ----------

#[derive(Debug, Clone, PartialEq)]
struct Visible {
    head: (String, String, String, bool, bool, bool),
    tree: Option<Node>,
}

#[derive(Debug, Clone, PartialEq)]
struct Node {
    kind: String,
    operand: String,
    sensitive: bool,
    value: Option<String>,
    members: Vec<Node>,
}

struct Lcg(u64);
impl Lcg {
    fn next(&mut self, n: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.0 >> 33) as usize) % n
    }
}

const WORDS: [&str; 7] = ["", "a", "ab", "\0", "\0\0\0\x01", "x.y", "/p"];
const OPERANDS: [&str; 8] = [
    "dest.host",
    "process.path",
    "dest.port",
    "process.command",
    "lists.ips",
    "true",
    "list",
    "weird",
];

fn gen_op(rng: &mut Lcg, depth: usize) -> (Operator, Node) {
    let operand = OPERANDS[rng.next(OPERANDS.len())];
    let data = WORDS[rng.next(WORDS.len())];
    let sensitive = rng.next(2) == 1;
    let mut o = Operator {
        sensitive,
        ..op("simple", operand, data)
    };
    let kept = ["dest.host", "process.path", "dest.port", "lists.ips"];
    let value = if kept.contains(&operand) {
        Some(data.to_owned())
    } else {
        None
    };
    let kind = if operand == "weird" {
        "[unknown]"
    } else {
        operand
    };
    let mut members = Vec::new();
    let mut nodes = Vec::new();
    if depth < 2 {
        for _ in 0..rng.next(3) {
            let (m, n) = gen_op(rng, depth + 1);
            members.push(m);
            nodes.push(n);
        }
    }
    o.list = members;
    // Withheld and structural operators both carry no value in the oracle;
    // they are told apart by kind (the oracle's `kind` for "true"/"list" is
    // their own name, for withheld kinds their own name too).
    let node = Node {
        kind: "simple".into(),
        operand: kind.to_owned(),
        sensitive,
        value,
        members: nodes,
    };
    (o, node)
}

#[test]
fn canonical_bytes_are_equal_exactly_when_the_visible_forms_are() {
    let mut rng = Lcg(7);
    let mut seen: Vec<(Vec<u8>, Visible)> = Vec::new();
    for _ in 0..400 {
        let name = WORDS[rng.next(WORDS.len())];
        let (o, node) = gen_op(&mut rng, 0);
        let r = Rule {
            name: name.into(),
            action: "allow".into(),
            duration: "always".into(),
            enabled: true,
            operator: Some(o),
            ..Rule::default()
        };
        let visible = Visible {
            head: (
                name.into(),
                "allow".into(),
                "always".into(),
                true,
                false,
                false,
            ),
            tree: Some(node),
        };
        seen.push((identify(&r).expect("identity").canonical, visible));
    }
    for (i, (ca, va)) in seen.iter().enumerate() {
        for (cb, vb) in &seen[i + 1..] {
            assert_eq!(ca == cb, va == vb, "{va:?} vs {vb:?}");
        }
    }
}

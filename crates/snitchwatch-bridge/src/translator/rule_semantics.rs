//! Bidirectional LS rule ↔ OpenSnitch rule translation.

use crate::translator::{glob, specificity};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LsRule {
    pub id: String,
    pub name: String,
    pub verdict: LsVerdict,
    pub direction: LsDirection,
    pub scope: LsScope,
    pub permanent: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LsVerdict {
    Allow,
    Deny,
    Blocklist,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LsDirection {
    Outgoing,
    Incoming,
    Both,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LsScope {
    pub process: Option<String>,
    pub remote_host: Option<RemoteHost>,
    pub port: Option<u16>,
    pub protocol: Option<Protocol>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RemoteHost {
    Exact(String),
    Glob(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Tcp,
    Udp,
    Any,
}

/// A friendlier wrapper around the proto-generated opensnitchd Rule type.
/// We don't use the proto type directly because (a) we want to test
/// translation without depending on tonic-build output and (b) we control
/// the field shape this way.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OsRule {
    pub name: String,
    pub enabled: bool,
    pub action: OsAction,
    pub duration: OsDuration,
    pub operator: OsOperator,
    /// `__source: "user"` or `"blocklist:<id>"` or `"unenforced:incoming"`.
    pub description: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OsAction {
    Allow,
    Deny,
    Reject,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OsDuration {
    Once,
    UntilRestart,
    Always,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OsOperator {
    Simple { operand: OsOperand, data: String },
    Regexp { operand: OsOperand, data: String },
    List { operands: Vec<OsOperator> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OsOperand {
    ProcessPath,
    DestHost,
    DestPort,
    Protocol,
    Direction,
}

#[derive(Debug, thiserror::Error)]
pub enum TranslateError {
    #[error("unsupported rule shape: {0}")]
    Unsupported(String),
}

/// Translate an LsRule into one or more OpenSnitch rules. Returns multiple
/// when `direction = Both` (one per direction).
pub fn ls_to_os(ls: &LsRule) -> Result<Vec<OsRule>, TranslateError> {
    let directions: Vec<LsDirection> = match ls.direction {
        LsDirection::Outgoing => vec![LsDirection::Outgoing],
        LsDirection::Incoming => vec![LsDirection::Incoming],
        LsDirection::Both => vec![LsDirection::Outgoing, LsDirection::Incoming],
    };

    let inputs = specificity::SpecificityInputs {
        has_process: ls.scope.process.is_some(),
        has_remote_host_exact: matches!(ls.scope.remote_host, Some(RemoteHost::Exact(_))),
        has_remote_host_glob: matches!(ls.scope.remote_host, Some(RemoteHost::Glob(_))),
        has_port: ls.scope.port.is_some(),
        has_protocol: !matches!(ls.scope.protocol, None | Some(Protocol::Any)),
    };
    let prefix = specificity::user_rule_prefix(&inputs);

    let action = match ls.verdict {
        LsVerdict::Allow => OsAction::Allow,
        LsVerdict::Deny | LsVerdict::Blocklist => OsAction::Deny,
    };

    let duration = if ls.permanent {
        OsDuration::Always
    } else {
        OsDuration::UntilRestart
    };

    let mut out = Vec::with_capacity(directions.len());

    for dir in directions {
        let mut operators: Vec<OsOperator> = Vec::new();

        if let Some(process) = &ls.scope.process {
            operators.push(OsOperator::Simple {
                operand: OsOperand::ProcessPath,
                data: process.clone(),
            });
        }

        if let Some(host) = &ls.scope.remote_host {
            match host {
                RemoteHost::Exact(h) => operators.push(OsOperator::Simple {
                    operand: OsOperand::DestHost,
                    data: h.clone(),
                }),
                RemoteHost::Glob(g) => operators.push(OsOperator::Regexp {
                    operand: OsOperand::DestHost,
                    data: glob::glob_to_regex_string(g),
                }),
            }
        }

        if let Some(port) = ls.scope.port {
            operators.push(OsOperator::Simple {
                operand: OsOperand::DestPort,
                data: port.to_string(),
            });
        }

        if let Some(proto) = ls.scope.protocol {
            if !matches!(proto, Protocol::Any) {
                operators.push(OsOperator::Simple {
                    operand: OsOperand::Protocol,
                    data: match proto {
                        Protocol::Tcp => "tcp".to_string(),
                        Protocol::Udp => "udp".to_string(),
                        Protocol::Any => unreachable!(),
                    },
                });
            }
        }

        // Always add the direction operator so the daemon knows which chain
        // to evaluate this rule in.
        operators.push(OsOperator::Simple {
            operand: OsOperand::Direction,
            data: match dir {
                LsDirection::Outgoing => "outgoing".to_string(),
                LsDirection::Incoming => "incoming".to_string(),
                LsDirection::Both => unreachable!(),
            },
        });

        let operator = if operators.len() == 1 {
            operators.into_iter().next().unwrap()
        } else {
            OsOperator::List {
                operands: operators,
            }
        };

        // Mark incoming-direction rules as unenforced because v1 of the
        // bridge does not enable opensnitchd's incoming hook by default.
        let description = if matches!(dir, LsDirection::Incoming) {
            format!(
                r#"{{"snitchwatch":{{"source":"user","ls_rule_id":"{}","status":"unenforced"}}}}"#,
                ls.id
            )
        } else {
            format!(
                r#"{{"snitchwatch":{{"source":"user","ls_rule_id":"{}","status":"enforced"}}}}"#,
                ls.id
            )
        };

        let dir_suffix = match dir {
            LsDirection::Outgoing => "out",
            LsDirection::Incoming => "in",
            LsDirection::Both => unreachable!(),
        };

        let name = format!("{}-{}-{}.json", prefix, slugify(&ls.name), dir_suffix);

        out.push(OsRule {
            name,
            enabled: true,
            action,
            duration,
            operator,
            description,
        });
    }

    Ok(out)
}

fn slugify(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process_only_allow() -> LsRule {
        LsRule {
            id: "r1".to_string(),
            name: "firefox-allow".to_string(),
            verdict: LsVerdict::Allow,
            direction: LsDirection::Outgoing,
            scope: LsScope {
                process: Some("/usr/bin/firefox".to_string()),
                remote_host: None,
                port: None,
                protocol: None,
            },
            permanent: true,
        }
    }

    #[test]
    fn process_only_allow_translates_to_single_os_rule() {
        let ls = process_only_allow();
        let out = ls_to_os(&ls).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].action, OsAction::Allow);
        assert_eq!(out[0].duration, OsDuration::Always);
        assert!(
            out[0].name.starts_with("899-"),
            "process-only score=100, prefix=899: {}",
            out[0].name
        );
        match &out[0].operator {
            OsOperator::List { operands } => {
                assert!(operands.iter().any(|op| matches!(
                    op,
                    OsOperator::Simple { operand: OsOperand::ProcessPath, data } if data == "/usr/bin/firefox"
                )));
                assert!(operands.iter().any(|op| matches!(
                    op,
                    OsOperator::Simple { operand: OsOperand::Direction, data } if data == "outgoing"
                )));
            }
            other => panic!("expected List, got {:?}", other),
        }
    }

    fn deny_with_glob_host() -> LsRule {
        LsRule {
            id: "r2".to_string(),
            name: "block-tracker".to_string(),
            verdict: LsVerdict::Deny,
            direction: LsDirection::Outgoing,
            scope: LsScope {
                process: None,
                remote_host: Some(RemoteHost::Glob("*.tracker.example".to_string())),
                port: None,
                protocol: None,
            },
            permanent: true,
        }
    }

    #[test]
    fn glob_host_becomes_regexp_operator() {
        let out = ls_to_os(&deny_with_glob_host()).unwrap();
        assert_eq!(out.len(), 1);
        let host_op = match &out[0].operator {
            OsOperator::List { operands } => operands
                .iter()
                .find(|o| {
                    matches!(
                        o,
                        OsOperator::Regexp {
                            operand: OsOperand::DestHost,
                            ..
                        }
                    )
                })
                .expect("must contain a Regexp host operand"),
            _ => panic!("expected List"),
        };
        if let OsOperator::Regexp { data, .. } = host_op {
            assert_eq!(data, r"^[^.]*\.tracker\.example$");
        }
    }

    #[test]
    fn both_direction_yields_two_rules() {
        let mut ls = process_only_allow();
        ls.direction = LsDirection::Both;
        let out = ls_to_os(&ls).unwrap();
        assert_eq!(out.len(), 2);
        let names: Vec<&str> = out.iter().map(|r| r.name.as_str()).collect();
        assert!(names.iter().any(|n| n.ends_with("-out.json")));
        assert!(names.iter().any(|n| n.ends_with("-in.json")));
    }

    #[test]
    fn incoming_rule_is_marked_unenforced() {
        let mut ls = process_only_allow();
        ls.direction = LsDirection::Incoming;
        let out = ls_to_os(&ls).unwrap();
        assert!(out[0].description.contains(r#""status":"unenforced""#));
    }

    #[test]
    fn outgoing_rule_is_marked_enforced() {
        let out = ls_to_os(&process_only_allow()).unwrap();
        assert!(out[0].description.contains(r#""status":"enforced""#));
    }

    #[test]
    fn session_rule_uses_until_restart_duration() {
        let mut ls = process_only_allow();
        ls.permanent = false;
        let out = ls_to_os(&ls).unwrap();
        assert_eq!(out[0].duration, OsDuration::UntilRestart);
    }

    #[test]
    fn blocklist_verdict_becomes_deny_action() {
        let mut ls = process_only_allow();
        ls.verdict = LsVerdict::Blocklist;
        let out = ls_to_os(&ls).unwrap();
        assert_eq!(out[0].action, OsAction::Deny);
    }

    #[test]
    fn protocol_any_is_omitted() {
        let mut ls = process_only_allow();
        ls.scope.protocol = Some(Protocol::Any);
        let out = ls_to_os(&ls).unwrap();
        if let OsOperator::List { operands } = &out[0].operator {
            assert!(!operands.iter().any(|op| matches!(
                op,
                OsOperator::Simple {
                    operand: OsOperand::Protocol,
                    ..
                }
            )));
        }
    }
}

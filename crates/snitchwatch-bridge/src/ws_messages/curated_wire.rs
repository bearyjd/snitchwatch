//! Strict raw-frame boundary for modern consent operations. The top-level
//! visitor preserves duplicate keys before any serde_json value map exists.
use super::{valid_request_id, ClientMessage};
use serde::de::{MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

pub const MAX_INTENT_MESSAGE_BYTES: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CuratedIntentPurpose {
    Enable,
    Reenable,
    Keep,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CuratedRequestOutcome {
    ChoiceAccepted,
    RefusedConsent,
    UnknownId,
    Unavailable,
    InvalidIntent,
    StaleIntent,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CuratedRequestResult {
    pub id: String,
    pub outcome: CuratedRequestOutcome,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub choice_accepted: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persisted: Option<bool>,
}

/// Fixed, non-secret failure information; never holds the submitted frame.
#[derive(Clone, PartialEq, Eq)]
pub struct IntentDecodeError {
    pub modern: bool,
    pub export: Option<crate::analysis_export::ExportError>,
    pub request_id: Option<String>,
}

impl std::fmt::Debug for IntentDecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IntentDecodeError")
            .field("modern", &self.modern)
            .field("export", &self.export)
            .finish_non_exhaustive()
    }
}

fn modern_action(action: &str) -> bool {
    matches!(
        action,
        "prepareCuratedBroadOptIn"
            | "enableCuratedBroadDefault"
            | "keepCuratedBroadDefault"
            | "cancelCuratedBroadOptIn"
    )
}

pub(super) struct RawFields(Vec<(String, Value)>, bool);

// Preserve nested duplicate evidence for export DTOs while keeping legacy
// JSON-map semantics for clients that predate the strict export protocol.
struct CheckedValue {
    value: Value,
    duplicate: bool,
}
impl<'de> Deserialize<'de> for CheckedValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ValueVisitor;
        impl<'de> Visitor<'de> for ValueVisitor {
            type Value = CheckedValue;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a JSON value")
            }
            fn visit_bool<E: serde::de::Error>(self, value: bool) -> Result<CheckedValue, E> {
                Ok(CheckedValue {
                    value: Value::Bool(value),
                    duplicate: false,
                })
            }
            fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<CheckedValue, E> {
                Ok(CheckedValue {
                    value: value.into(),
                    duplicate: false,
                })
            }
            fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<CheckedValue, E> {
                Ok(CheckedValue {
                    value: value.into(),
                    duplicate: false,
                })
            }
            fn visit_f64<E: serde::de::Error>(self, value: f64) -> Result<CheckedValue, E> {
                Ok(CheckedValue {
                    value: serde_json::Number::from_f64(value)
                        .map(Value::Number)
                        .ok_or_else(|| E::custom("invalid number"))?,
                    duplicate: false,
                })
            }
            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<CheckedValue, E> {
                self.visit_string(value.to_owned())
            }
            fn visit_string<E: serde::de::Error>(self, value: String) -> Result<CheckedValue, E> {
                Ok(CheckedValue {
                    value: Value::String(value),
                    duplicate: false,
                })
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<CheckedValue, E> {
                Ok(CheckedValue {
                    value: Value::Null,
                    duplicate: false,
                })
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<CheckedValue, A::Error> {
                let mut values = Vec::new();
                let mut duplicate = false;
                while let Some(child) = seq.next_element::<CheckedValue>()? {
                    duplicate |= child.duplicate;
                    values.push(child.value);
                }
                Ok(CheckedValue {
                    value: Value::Array(values),
                    duplicate,
                })
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<CheckedValue, A::Error> {
                let mut values = serde_json::Map::new();
                let mut duplicate = false;
                while let Some((key, child)) = map.next_entry::<String, CheckedValue>()? {
                    duplicate |= child.duplicate;
                    duplicate |= values.insert(key, child.value).is_some();
                }
                Ok(CheckedValue {
                    value: Value::Object(values),
                    duplicate,
                })
            }
        }
        deserializer.deserialize_any(ValueVisitor)
    }
}
impl<'de> Deserialize<'de> for RawFields {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct FieldsVisitor;
        impl<'de> Visitor<'de> for FieldsVisitor {
            type Value = RawFields;
            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a client message object")
            }
            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<RawFields, M::Error> {
                let mut fields = Vec::new();
                let mut duplicate = false;
                while let Some((key, child)) = map.next_entry::<String, CheckedValue>()? {
                    duplicate |= child.duplicate;
                    fields.push((key, child.value));
                }
                Ok(RawFields(fields, duplicate))
            }
        }
        deserializer.deserialize_map(FieldsVisitor)
    }
}

#[derive(Deserialize)]
#[serde(
    tag = "action",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
enum ModernIntentDto {
    PrepareCuratedBroadOptIn {
        request_id: String,
        id: String,
        purpose: CuratedIntentPurpose,
    },
    EnableCuratedBroadDefault {
        request_id: String,
        id: String,
        purpose: CuratedIntentPurpose,
        ticket: String,
        consent: String,
    },
    KeepCuratedBroadDefault {
        request_id: String,
        id: String,
        purpose: CuratedIntentPurpose,
        ticket: String,
        consent: String,
    },
    CancelCuratedBroadOptIn {
        request_id: String,
        id: String,
    },
}

impl ModernIntentDto {
    fn into_message(self) -> Option<ClientMessage> {
        let context = None;
        let reply = None;
        Some(match self {
            Self::PrepareCuratedBroadOptIn {
                request_id,
                id,
                purpose,
            } if valid_request_id(&request_id) && id == "dns-resolved" => {
                ClientMessage::PrepareCuratedBroadOptIn {
                    request_id,
                    id,
                    purpose,
                    context,
                    reply,
                }
            }
            Self::EnableCuratedBroadDefault {
                request_id,
                id,
                purpose,
                ticket,
                consent,
            } if valid_submission(&request_id, &id, &ticket, &consent)
                && purpose != CuratedIntentPurpose::Keep =>
            {
                ClientMessage::EnableCuratedBroadDefault {
                    request_id,
                    id,
                    purpose,
                    ticket,
                    consent,
                    context,
                    reply,
                }
            }
            Self::KeepCuratedBroadDefault {
                request_id,
                id,
                purpose,
                ticket,
                consent,
            } if valid_submission(&request_id, &id, &ticket, &consent)
                && purpose == CuratedIntentPurpose::Keep =>
            {
                ClientMessage::KeepCuratedBroadDefault {
                    request_id,
                    id,
                    purpose,
                    ticket,
                    consent,
                    context,
                    reply,
                }
            }
            Self::CancelCuratedBroadOptIn { request_id, id }
                if valid_request_id(&request_id) && id == "dns-resolved" =>
            {
                ClientMessage::CancelCuratedBroadOptIn {
                    request_id,
                    id,
                    context,
                    reply,
                }
            }
            _ => return None,
        })
    }
}

fn valid_submission(request_id: &str, id: &str, ticket: &str, consent: &str) -> bool {
    valid_request_id(request_id)
        && id == "dns-resolved"
        && crate::curated::intent::valid_ticket(ticket)
        && consent == crate::curated::intent::CONSENT_VERSION
}

/// Production text boundary: export frames receive a streaming classification
/// pass and an 8 KiB raw byte cap before strict DTO materialization. Programmatic
/// `ClientMessage` serde cannot measure original whitespace/escaping bytes; it
/// preserves the same strict export semantics and duplicate-field rejection.
pub fn decode_client_message(text: &str) -> Result<ClientMessage, IntentDecodeError> {
    if let Some(result) = super::analysis_export_wire::decode_export_frame(text) {
        return result;
    }
    let fields = serde_json::from_str::<RawFields>(text).map_err(|_| IntentDecodeError {
        modern: false,
        export: None,
        request_id: None,
    })?;
    let result = decode_fields(fields);
    if text.len() > MAX_INTENT_MESSAGE_BYTES {
        match result {
            Ok(
                ClientMessage::PrepareCuratedBroadOptIn { request_id, .. }
                | ClientMessage::EnableCuratedBroadDefault { request_id, .. }
                | ClientMessage::KeepCuratedBroadDefault { request_id, .. }
                | ClientMessage::CancelCuratedBroadOptIn { request_id, .. },
            ) => {
                return Err(IntentDecodeError {
                    modern: true,
                    export: None,
                    request_id: Some(request_id),
                })
            }
            other => return other,
        }
    }
    result
}

pub(super) fn decode_fields(fields: RawFields) -> Result<ClientMessage, IntentDecodeError> {
    let nested_duplicate = fields.1;
    let fields = fields.0;
    if fields.iter().any(|(key, value)| {
        key == "action"
            && value
                .as_str()
                .is_some_and(super::analysis_export_wire::export_action)
    }) {
        return super::analysis_export_wire::decode_export_fields(fields, nested_duplicate);
    }
    let modern = fields
        .iter()
        .any(|(key, value)| key == "action" && value.as_str().is_some_and(modern_action));
    if !modern {
        return super::ClientMessageSerde::deserialize(serde::de::value::MapDeserializer::new(
            fields.into_iter(),
        ))
        .map_err(|_| IntentDecodeError {
            modern: false,
            export: None,
            request_id: None,
        });
    }
    let mut request_ids = fields.iter().filter(|(key, _)| key == "requestId");
    let request_id = request_ids
        .next()
        .and_then(|(_, value)| value.as_str())
        .filter(|id| valid_request_id(id))
        .map(str::to_owned);
    let request_id = if request_ids.next().is_none() {
        request_id
    } else {
        None
    };
    let error = IntentDecodeError {
        modern: true,
        export: None,
        request_id,
    };
    let mut map = serde_json::Map::new();
    for (key, value) in fields {
        if map.insert(key, value).is_some() {
            return Err(error);
        }
    }
    serde_json::from_value::<ModernIntentDto>(Value::Object(map))
        .ok()
        .and_then(ModernIntentDto::into_message)
        .ok_or(error)
}

//! Export frames are classified without materializing fields, then strictly decoded.
use super::{valid_request_id, ClientMessage, IntentDecodeError};
use crate::analysis_export::{ExportError, ExportFilters};
use serde::de::{IgnoredAny, MapAccess, Visitor};
use serde::{Deserialize, Deserializer};
use std::fmt;

const MAX_REQUEST_BYTES: usize = 8192;
pub(super) fn export_action(action: &str) -> bool {
    matches!(
        action,
        "requestCacheSummaryExport" | "cancelCacheSummaryExport"
    )
}
#[derive(Deserialize)]
#[serde(
    tag = "action",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
enum ExportDto {
    RequestCacheSummaryExport {
        request_id: String,
        schema_version: u64,
        filters: ExportFilters,
    },
    CancelCacheSummaryExport {
        request_id: String,
        schema_version: u64,
    },
}
impl ExportDto {
    fn into_message(self) -> Result<ClientMessage, ExportError> {
        let (request_id, schema_version) = match &self {
            Self::RequestCacheSummaryExport {
                request_id,
                schema_version,
                ..
            }
            | Self::CancelCacheSummaryExport {
                request_id,
                schema_version,
            } => (request_id, schema_version),
        };
        if !valid_request_id(request_id) {
            return Err(ExportError::InvalidRequest);
        }
        if *schema_version != 1 {
            return Err(ExportError::UnsupportedVersion);
        }
        Ok(match self {
            Self::RequestCacheSummaryExport {
                request_id,
                schema_version,
                filters,
            } => {
                filters.validate()?;
                ClientMessage::RequestCacheSummaryExport {
                    request_id,
                    schema_version,
                    filters,
                    reply: None,
                    session: None,
                }
            }
            Self::CancelCacheSummaryExport {
                request_id,
                schema_version,
            } => ClientMessage::CancelCacheSummaryExport {
                request_id,
                schema_version,
                reply: None,
                session: None,
            },
        })
    }
}
fn error(request_id: Option<String>, reason: ExportError) -> IntentDecodeError {
    IntentDecodeError {
        modern: false,
        export: Some(reason),
        request_id,
    }
}

/// Only action and a trustworthy small request id survive this pass. Unknown
/// values are walked with IgnoredAny; a giant selector is never allocated here.
#[derive(Default)]
struct Preflight {
    export: bool,
    actions: usize,
    ids: usize,
    request_id: Option<String>,
}
enum Field {
    Action,
    RequestId,
    Other,
}
impl<'de> Deserialize<'de> for Field {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct FieldVisitor;
        impl Visitor<'_> for FieldVisitor {
            type Value = Field;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a field")
            }
            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Field, E> {
                Ok(match value {
                    "action" => Field::Action,
                    "requestId" => Field::RequestId,
                    _ => Field::Other,
                })
            }
        }
        deserializer.deserialize_identifier(FieldVisitor)
    }
}
struct SmallToken(Option<String>);
impl<'de> Deserialize<'de> for SmallToken {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct TokenVisitor;
        impl<'de> Visitor<'de> for TokenVisitor {
            type Value = SmallToken;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a token")
            }
            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<SmallToken, E> {
                Ok(SmallToken((value.len() <= 64).then(|| value.to_owned())))
            }
            fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<SmallToken, E> {
                Ok(SmallToken(None))
            }
            fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<SmallToken, E> {
                Ok(SmallToken(None))
            }
            fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<SmallToken, E> {
                Ok(SmallToken(None))
            }
            fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<SmallToken, E> {
                Ok(SmallToken(None))
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<SmallToken, E> {
                Ok(SmallToken(None))
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<SmallToken, A::Error> {
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                Ok(SmallToken(None))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<SmallToken, A::Error> {
                while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                Ok(SmallToken(None))
            }
        }
        deserializer.deserialize_any(TokenVisitor)
    }
}
struct PreflightVisitor<'a>(&'a mut Preflight);
impl<'de> Visitor<'de> for PreflightVisitor<'_> {
    type Value = ();
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a client object")
    }
    fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<(), M::Error> {
        while let Some(key) = map.next_key::<Field>()? {
            match key {
                Field::Action => {
                    self.0.actions += 1;
                    let token = map.next_value::<SmallToken>()?;
                    self.0.export |= token.0.as_deref().is_some_and(export_action);
                }
                Field::RequestId => {
                    self.0.ids += 1;
                    let token = map.next_value::<SmallToken>()?;
                    self.0.request_id = token.0.filter(|value| valid_request_id(value));
                }
                Field::Other => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(())
    }
}

/// None leaves curated and legacy parsing unchanged. Some always classifies an
/// export as private, including duplicate actions and malformed trailing input.
pub(super) fn decode_export_frame(text: &str) -> Option<Result<ClientMessage, IntentDecodeError>> {
    let mut preflight = Preflight::default();
    let mut de = serde_json::Deserializer::from_str(text);
    let complete = de
        .deserialize_map(PreflightVisitor(&mut preflight))
        .and_then(|()| de.end())
        .is_ok();
    if !preflight.export {
        return None;
    }
    let request_id = if complete && preflight.actions == 1 && preflight.ids == 1 {
        preflight.request_id
    } else {
        None
    };
    if !complete || text.len() > MAX_REQUEST_BYTES || preflight.actions != 1 || preflight.ids != 1 {
        return Some(Err(error(request_id, ExportError::InvalidRequest)));
    }
    Some(
        serde_json::from_str::<ExportDto>(text)
            .map_err(|_| ExportError::InvalidRequest)
            .and_then(ExportDto::into_message)
            .map_err(|reason| error(request_id, reason)),
    )
}

/// Generic serde entry point uses duplicate-preserving top-level fields and a
/// duplicate flag from their recursive reader. The raw byte cap belongs to
/// decode_export_frame; arbitrary serde Deserializers have no raw byte span.
pub(super) fn decode_export_fields(
    fields: Vec<(String, serde_json::Value)>,
    nested_duplicate: bool,
) -> Result<ClientMessage, IntentDecodeError> {
    let mut ids = fields.iter().filter(|(key, _)| key == "requestId");
    let request_id = ids
        .next()
        .and_then(|(_, value)| value.as_str())
        .filter(|id| valid_request_id(id))
        .map(str::to_owned);
    let request_id =
        if ids.next().is_none() && fields.iter().filter(|(key, _)| key == "action").count() == 1 {
            request_id
        } else {
            None
        };
    if nested_duplicate {
        return Err(error(request_id, ExportError::InvalidRequest));
    }
    ExportDto::deserialize(serde::de::value::MapDeserializer::new(fields.into_iter()))
        .map_err(|_| ExportError::InvalidRequest)
        .and_then(ExportDto::into_message)
        .map_err(|reason| error(request_id, reason))
}

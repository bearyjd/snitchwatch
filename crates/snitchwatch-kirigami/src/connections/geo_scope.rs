//! Typed session-only geography. Keys come from the shared resolver hierarchy.
use std::net::IpAddr;

use crate::geo::{detail::country_key, resolver::Resolved};
use snitchwatch_bridge::ws_messages::ConnectionRow;

/// Compact retained identity: no MMDB access or composite-key serialization
/// is needed while repeatedly filtering the Qt-side row store.
#[derive(Debug, Clone)]
pub(crate) struct GeoIdentity {
    pub country_key: String,
    pub city_key: String,
}
impl From<Resolved> for GeoIdentity {
    fn from(resolved: Resolved) -> Self {
        let country_key = country_key(&resolved.bucket);
        let city_key = resolved
            .place
            .as_ref()
            .filter(|place| place.city.is_some())
            .map(|place| place.city_key())
            .unwrap_or_else(|| format!("{country_key}:unknown-city"));
        Self {
            country_key,
            city_key,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GeoScope {
    Country(String),
    City(String),
    Ip(IpAddr),
}

impl GeoScope {
    pub fn parse(kind: &str, key: &str) -> Option<Self> {
        match kind {
            "country" if valid_country(key) => Some(Self::Country(key.into())),
            "city"
                if key.len() <= 8192
                    && ((key.starts_with("city:") && key.len() > 5)
                        || key.strip_suffix(":unknown-city").is_some_and(valid_country)) =>
            {
                Some(Self::City(key.into()))
            }
            "ip" => key.strip_prefix("ip:")?.parse().ok().map(Self::Ip),
            _ => None,
        }
    }
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Country(_) => "country",
            Self::City(_) => "city",
            Self::Ip(_) => "ip",
        }
    }
    pub fn key(&self) -> String {
        match self {
            Self::Country(key) | Self::City(key) => key.clone(),
            Self::Ip(ip) => format!("ip:{ip}"),
        }
    }
    pub fn description(&self) -> String {
        match self {
            Self::Country(key) if key == "local" => "local network".into(),
            Self::Country(key) if key == "unknown" => "unknown location".into(),
            Self::Country(key) => {
                format!("country {}", key.strip_prefix("country:").unwrap_or(key))
            }
            Self::City(_) => "selected city / unavailable-city branch".into(),
            Self::Ip(ip) => format!("exact IP {ip}"),
        }
    }
    pub(crate) fn matches(&self, row: &ConnectionRow, identity: &GeoIdentity) -> bool {
        match self {
            Self::Country(key) => identity.country_key == *key,
            Self::City(key) => identity.city_key == *key,
            Self::Ip(ip) => row
                .dst_ip
                .parse::<IpAddr>()
                .is_ok_and(|actual| actual == *ip),
        }
    }
}

fn valid_country(key: &str) -> bool {
    matches!(key, "local" | "unknown")
        || key
            .strip_prefix("country:")
            .is_some_and(|code| code.len() == 2 && code.bytes().all(|c| c.is_ascii_uppercase()))
}

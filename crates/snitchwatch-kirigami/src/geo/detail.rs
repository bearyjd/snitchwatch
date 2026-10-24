//! Observed geographic hierarchy. Counts describe cached connection rows only.
//! Keys are opaque, stable for the active database snapshot, and never positions.
use std::collections::BTreeMap;
use std::net::IpAddr;

use serde::Serialize;
use snitchwatch_bridge::ws_messages::ConnectionRow;

use super::resolver::{Bucket, GeoCoordinates, GeoPlace, Resolved};
use crate::connections::row_store::Verdict;

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Counts {
    pub total: u32,
    pub allowed: u32,
    pub denied: u32,
    pub pending: u32,
}
impl Counts {
    fn add(&mut self, verdict: Verdict) {
        self.total += 1;
        match verdict {
            Verdict::Allowed => self.allowed += 1,
            Verdict::Denied | Verdict::Blocklisted => self.denied += 1,
            Verdict::Pending => self.pending += 1,
            Verdict::Other => {}
        }
    }
}

#[derive(Clone)]
pub(crate) struct Observation {
    pub resolved: Resolved,
    pub canonical_ip: Option<String>,
    pub address: String,
    pub facts: ConnectionFacts,
    pub verdict: Verdict,
}
impl Observation {
    pub fn new(row: &ConnectionRow, resolved: Resolved) -> Self {
        Self {
            canonical_ip: row.dst_ip.parse::<IpAddr>().ok().map(|ip| ip.to_string()),
            address: row.dst_ip.clone(),
            resolved,
            verdict: Verdict::of(row),
            facts: ConnectionFacts {
                id: row.id.clone(),
                app: row.process.clone(),
                app_path: row.process_path.clone(),
                host: row.dst_host.clone(),
                port: row.dst_port,
                protocol: row.protocol.clone(),
                action: row.action.clone(),
            },
        }
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionFacts {
    pub id: String,
    pub app: String,
    pub app_path: Option<String>,
    pub host: String,
    pub port: u16,
    pub protocol: String,
    pub action: Option<String>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObservedIp {
    pub key: String,
    pub canonical_ip: Option<String>,
    pub address: String,
    pub coordinates: Option<GeoCoordinates>,
    pub counts: Counts,
    pub connections: Vec<ConnectionFacts>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CityNode {
    pub key: String,
    pub name: String,
    pub city_available: bool,
    pub place: Option<GeoPlace>,
    pub counts: Counts,
    pub ips: BTreeMap<String, ObservedIp>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CountryNode {
    pub key: String,
    pub code: Option<String>,
    pub name: String,
    pub counts: Counts,
    pub cities: BTreeMap<String, CityNode>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GeoHierarchy {
    pub version: u8,
    pub observed_connections: usize,
    pub capacity_reached: bool,
    pub countries: BTreeMap<String, CountryNode>,
}

pub fn country_key(bucket: &Bucket) -> String {
    match bucket {
        Bucket::Country(code) => format!("country:{code}"),
        Bucket::Local => "local".into(),
        Bucket::Unknown => "unknown".into(),
    }
}

pub(crate) fn hierarchy<'a>(
    observations: impl Iterator<Item = &'a Observation>,
    capacity_reached: bool,
) -> GeoHierarchy {
    let mut result = GeoHierarchy {
        version: 1,
        observed_connections: 0,
        capacity_reached,
        countries: BTreeMap::new(),
    };
    for observation in observations {
        result.observed_connections += 1;
        let resolved = &observation.resolved;
        let country_key = country_key(&resolved.bucket);
        let city_available = resolved.place.as_ref().is_some_and(|p| p.city.is_some());
        let city_key = if city_available {
            resolved.place.as_ref().unwrap().city_key()
        } else {
            format!("{country_key}:unknown-city")
        };
        let ip_key = observation
            .canonical_ip
            .as_ref()
            .map(|ip| format!("ip:{ip}"))
            .unwrap_or_else(|| {
                format!(
                    "invalid:{}",
                    serde_json::to_string(&observation.address).unwrap()
                )
            });
        let country = result
            .countries
            .entry(country_key.clone())
            .or_insert_with(|| CountryNode {
                key: country_key,
                code: match &resolved.bucket {
                    Bucket::Country(code) => Some(code.clone()),
                    _ => None,
                },
                name: resolved.display_name.clone(),
                counts: Counts::default(),
                cities: BTreeMap::new(),
            });
        country.counts.add(observation.verdict);
        let city = country
            .cities
            .entry(city_key.clone())
            .or_insert_with(|| CityNode {
                key: city_key,
                name: resolved
                    .place
                    .as_ref()
                    .and_then(|p| p.city.clone())
                    .unwrap_or_else(|| "City unavailable".into()),
                city_available,
                place: if city_available {
                    resolved.place.clone()
                } else {
                    None
                },
                counts: Counts::default(),
                ips: BTreeMap::new(),
            });
        city.counts.add(observation.verdict);
        let ip = city
            .ips
            .entry(ip_key.clone())
            .or_insert_with(|| ObservedIp {
                key: ip_key,
                canonical_ip: observation.canonical_ip.clone(),
                address: observation
                    .canonical_ip
                    .clone()
                    .unwrap_or_else(|| observation.address.clone()),
                coordinates: resolved.place.as_ref().and_then(|p| p.coordinates.clone()),
                counts: Counts::default(),
                connections: Vec::new(),
            });
        ip.counts.add(observation.verdict);
        ip.connections.push(observation.facts.clone());
    }
    for country in result.countries.values_mut() {
        for city in country.cities.values_mut() {
            for ip in city.ips.values_mut() {
                ip.connections.sort_by(|a, b| a.id.cmp(&b.id));
            }
        }
    }
    result
}

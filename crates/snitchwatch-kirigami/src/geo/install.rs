//! Explicit, bounded installation of a user-selected, extracted Country DB.

use std::io::Read;
use std::path::{Path, PathBuf};

const MAX_DATABASE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_NETWORKS: usize = 2_000_000;

mod tree;

/// Always install in app data, never in a system database or an env override.
pub fn target_path() -> PathBuf {
    super::paths::default_suggested_path(
        &crate::paths::home_base_rechecked(),
        std::env::var("XDG_DATA_HOME").ok().as_deref(),
    )
}

pub fn install(source: &Path, target: &Path) -> Result<(), String> {
    let file = crate::logging::open_no_follow(source, std::fs::OpenOptions::new().read(true))
        .map_err(|e| format!("Could not open the selected file: {e}"))?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.len() > MAX_DATABASE_BYTES {
        return Err("Choose a regular, extracted .mmdb file no larger than 32 MiB.".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_DATABASE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("Could not read the selected file: {e}"))?;
    if bytes.len() as u64 > MAX_DATABASE_BYTES {
        return Err("The selected database exceeds 32 MiB.".into());
    }
    validate(&bytes)?;
    crate::private_files::write_private(target, &bytes)
        .map_err(|e| format!("Could not install the database: {e}"))
}

fn validate(bytes: &[u8]) -> Result<(), String> {
    let invalid = |e| format!("The selected file is not a valid country database: {e}");
    let reader = maxminddb::Reader::from_source(bytes).map_err(invalid)?;
    if !matches!(
        reader.metadata().database_type.as_str(),
        "DBIP-Country-Lite" | "GeoLite2-Country" | "GeoIP2-Country"
    ) {
        return Err(
            "Choose an extracted DB-IP Country Lite, GeoLite2-Country or GeoIP2-Country .mmdb file.".into(),
        );
    }
    // The library iterator does not bound internal-node traversal between
    // yielded records. Validate its entire reachable graph before calling it.
    // Allow empty leaves and shared IPv4 aliases as well as data records.
    tree::validate(bytes, reader.metadata(), MAX_NETWORKS * 4 + 1)?;
    let mut countries = false;
    for (count, result) in reader
        .networks(Default::default())
        .map_err(invalid)?
        .enumerate()
    {
        if count >= MAX_NETWORKS {
            return Err("The selected database contains too many network records.".into());
        }
        let result = result.map_err(invalid)?;
        let country: Option<maxminddb::geoip2::Country> = result.decode().map_err(invalid)?;
        if let Some(code) = country.and_then(|c| c.country.iso_code) {
            if code.len() != 2 || !code.bytes().all(|c| c.is_ascii_uppercase()) {
                return Err("The selected database contains an invalid country code.".into());
            }
            countries = true;
        }
    }
    if !countries {
        return Err("The selected database contains no country records.".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests;

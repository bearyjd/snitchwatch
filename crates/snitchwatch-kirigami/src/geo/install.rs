//! Explicit, bounded installation of a user-selected, extracted Country DB.

use std::io::Read;
use std::path::{Path, PathBuf};

#[cfg(test)]
const MAX_DATABASE_BYTES: u64 = 32 * 1024 * 1024;

mod tree;
mod worker;
pub use worker::DatabaseProfile;

/// Reserved subprocess mode, handled before logging, Qt, bridge or threads.
/// The worker receives bytes only, never filenames or privileged operations.
pub fn validation_worker_entry() -> Option<i32> {
    let mut args = std::env::args_os().skip(1);
    let profile = match args.next().as_deref() {
        Some(flag) if flag == std::ffi::OsStr::new(worker::FLAG) => DatabaseProfile::Country,
        Some(flag) if flag == std::ffi::OsStr::new(worker::CITY_FLAG) => DatabaseProfile::City,
        _ => return None,
    };
    if args.next().is_some() {
        return Some(2);
    }
    Some(worker::run(profile, |bytes| {
        validate_profile(bytes, profile)
    }))
}

/// Always install in app data, never in a system database or an env override.
pub fn target_path() -> PathBuf {
    super::paths::default_suggested_path(
        &crate::paths::home_base_rechecked(),
        std::env::var("XDG_DATA_HOME").ok().as_deref(),
    )
}

pub fn install(source: &Path, target: &Path) -> Result<(), String> {
    install_with_validator(source, target, worker::validate)
}

pub fn target_path_for(profile: DatabaseProfile) -> PathBuf {
    match profile {
        DatabaseProfile::Country => target_path(),
        DatabaseProfile::City => target_path().with_file_name("dbip-city-lite.mmdb"),
    }
}

/// Selection is written only after validation and atomic dataset installation.
/// If selection fails, startup continues using the previous selection.
pub fn install_selected(source: &Path, profile: DatabaseProfile) -> Result<(), String> {
    static INSTALL_BUSY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    struct BusyGuard;
    impl Drop for BusyGuard {
        fn drop(&mut self) {
            INSTALL_BUSY.store(false, std::sync::atomic::Ordering::Release);
        }
    }
    if INSTALL_BUSY
        .compare_exchange(
            false,
            true,
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
        )
        .is_err()
    {
        return Err("Another geographic database import is already running.".into());
    }
    let _guard = BusyGuard;
    install_selected_with_validator(source, &target_path_for(profile), profile, |bytes| {
        worker::validate_profile(bytes, profile)
    })
}

fn install_selected_with_validator(
    source: &Path,
    target: &Path,
    profile: DatabaseProfile,
    validate: impl FnOnce(&[u8]) -> Result<(), String>,
) -> Result<(), String> {
    install_transaction(
        source,
        target,
        profile,
        validate,
        crate::private_files::write_private,
    )
}

fn install_transaction(
    source: &Path,
    target: &Path,
    profile: DatabaseProfile,
    validate: impl FnOnce(&[u8]) -> Result<(), String>,
    write_manifest: impl FnOnce(&Path, &[u8]) -> std::io::Result<()>,
) -> Result<(), String> {
    let directory = target
        .parent()
        .ok_or("Database storage has no parent directory.")?;
    let staged = super::dataset::reserve(directory)?;
    let result = install_profile_with_validator(source, &staged, profile, validate)
        .and_then(|()| super::dataset::commit(directory, &staged, profile, write_manifest));
    if result.is_err() {
        let _ = std::fs::remove_file(&staged);
    }
    result
}

fn install_with_validator(
    source: &Path,
    target: &Path,
    validate: impl FnOnce(&[u8]) -> Result<(), String>,
) -> Result<(), String> {
    install_profile_with_validator(source, target, DatabaseProfile::Country, validate)
}

fn install_profile_with_validator(
    source: &Path,
    target: &Path,
    profile: DatabaseProfile,
    validate: impl FnOnce(&[u8]) -> Result<(), String>,
) -> Result<(), String> {
    let max_bytes = profile.max_input() as u64;
    let file = crate::logging::open_no_follow(source, std::fs::OpenOptions::new().read(true))
        .map_err(|e| format!("Could not open the selected file: {e}"))?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.len() > max_bytes {
        return Err(profile.size_error());
    }
    let mut bytes = Vec::new();
    file.take(max_bytes + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("Could not read the selected file: {e}"))?;
    if bytes.len() as u64 > max_bytes {
        return Err(profile.size_error());
    }
    validate(&bytes)?;
    crate::private_files::write_private(target, &bytes)
        .map_err(|e| format!("Could not install the database: {e}"))
}

#[cfg(test)]
fn validate_in_worker(bytes: &[u8]) -> Result<(), String> {
    validate_profile(bytes, DatabaseProfile::Country)
}

fn validate_profile(bytes: &[u8], profile: DatabaseProfile) -> Result<(), String> {
    let invalid = |e| format!("The selected file is not a valid geographic database: {e}");
    let reader = maxminddb::Reader::from_source(bytes).map_err(invalid)?;
    let kind = reader.metadata().database_type.as_str();
    let supported = match profile {
        DatabaseProfile::Country => matches!(
            kind,
            "DBIP-Country-Lite" | "GeoLite2-Country" | "GeoIP2-Country"
        ),
        DatabaseProfile::City => matches!(kind, "DBIP-City-Lite" | "GeoLite2-City" | "GeoIP2-City"),
    };
    if !supported {
        return Err(
            "Choose an extracted .mmdb database matching the selected Country or City import."
                .into(),
        );
    }
    // The library iterator does not bound internal-node traversal between
    // yielded records. Validate its entire reachable graph before calling it.
    // Allow empty leaves and shared IPv4 aliases as well as data records.
    tree::validate(bytes, reader.metadata(), profile.max_steps())?;
    let mut countries = false;
    for (count, result) in reader
        .networks(Default::default())
        .map_err(invalid)?
        .enumerate()
    {
        if count >= profile.max_networks() {
            return Err("The selected database contains too many network records.".into());
        }
        let result = result.map_err(invalid)?;
        let code = match profile {
            DatabaseProfile::Country => {
                let country: Option<maxminddb::geoip2::Country> =
                    result.decode().map_err(invalid)?;
                country.and_then(|c| c.country.iso_code)
            }
            DatabaseProfile::City => {
                let city: Option<maxminddb::geoip2::City> = result.decode().map_err(invalid)?;
                if let Some(city) = city {
                    validate_city(&city)?;
                    city.country.iso_code
                } else {
                    None
                }
            }
        };
        if let Some(code) = code {
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

fn validate_city(city: &maxminddb::geoip2::City<'_>) -> Result<(), String> {
    let text_valid = |text: &str| text.len() <= 256 && !text.trim().chars().any(char::is_control);
    let all_names = |names: &maxminddb::geoip2::Names<'_>| {
        [
            names.english,
            names.german,
            names.spanish,
            names.french,
            names.japanese,
            names.brazilian_portuguese,
            names.russian,
            names.simplified_chinese,
        ]
        .into_iter()
        .flatten()
        .all(text_valid)
    };
    if !all_names(&city.city.names) || !all_names(&city.country.names) {
        return Err("The database contains an invalid geographic name.".into());
    }
    let mut names = Vec::new();
    if city.subdivisions.len() > 8 {
        return Err("The database has too many subdivisions.".into());
    }
    for subdivision in &city.subdivisions {
        if !all_names(&subdivision.names) {
            return Err("The database contains an invalid geographic name.".into());
        }
        names.push(subdivision.iso_code);
    }
    if !names.into_iter().flatten().all(text_valid) {
        return Err("The database contains an invalid geographic name.".into());
    }
    if city
        .location
        .latitude
        .is_some_and(|x| !x.is_finite() || !(-90.0..=90.0).contains(&x))
        || city
            .location
            .longitude
            .is_some_and(|x| !x.is_finite() || !(-180.0..=180.0).contains(&x))
    {
        return Err("The database contains invalid geographic coordinates.".into());
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod test_fixtures;
#[cfg(test)]
mod tests;

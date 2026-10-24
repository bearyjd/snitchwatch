//! Atomic local dataset selection. An immutable imported generation becomes
//! active only when one small manifest is atomically replaced.
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::install::DatabaseProfile;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Manifest {
    pub profile: DatabaseProfile,
    pub file: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub city: Option<String>,
}

pub(crate) enum Selection {
    Automatic,
    Country,
    City,
    Installed {
        profile: DatabaseProfile,
        path: PathBuf,
    },
}

fn read_bounded(path: &Path) -> Option<Vec<u8>> {
    let file = crate::logging::open_no_follow(path, std::fs::OpenOptions::new().read(true)).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > 1024 {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(1025).read_to_end(&mut bytes).ok()?;
    (bytes.len() <= 1024).then_some(bytes)
}

fn valid_generation(name: &str) -> bool {
    name.strip_prefix("geo-import-")
        .and_then(|s| s.strip_suffix(".mmdb"))
        .is_some_and(|s| s.len() == 32 && s.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn read_manifest(directory: &Path) -> Option<Manifest> {
    let bytes = read_bounded(&directory.join("geo-dataset"))?;
    let manifest: Manifest = serde_json::from_slice(&bytes).ok()?;
    (valid_generation(&manifest.file)
        && manifest
            .country
            .as_ref()
            .is_none_or(|s| valid_generation(s))
        && manifest.city.as_ref().is_none_or(|s| valid_generation(s)))
    .then_some(manifest)
}

/// The latest imported Country generation remains the fallback even while
/// City is selected. The one manifest commits both identities together.
pub(crate) fn country_path(directory: &Path) -> Option<PathBuf> {
    let manifest = read_manifest(directory)?;
    let file = if manifest.profile == DatabaseProfile::Country {
        Some(manifest.file)
    } else {
        manifest.country
    }?;
    Some(directory.join(file))
}

pub(crate) fn read(directory: &Path) -> Selection {
    let Some(bytes) = read_bounded(&directory.join("geo-dataset")) else {
        return Selection::Automatic;
    };
    match bytes.as_slice() {
        b"country" => Selection::Country,
        b"city" => Selection::City,
        _ => match read_manifest(directory) {
            Some(manifest) => Selection::Installed {
                profile: manifest.profile,
                path: directory.join(manifest.file),
            },
            None => Selection::Automatic,
        },
    }
}

/// Reserve a private, unique path before writing, independent of input names.
pub(crate) fn reserve(directory: &Path) -> Result<PathBuf, String> {
    use std::os::unix::fs::OpenOptionsExt;
    crate::private_files::ensure_private_dir(directory).map_err(|e| e.to_string())?;
    let mut random = [0u8; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut random))
        .map_err(|e| format!("Could not reserve database storage: {e}"))?;
    let name = format!(
        "geo-import-{}.mmdb",
        random
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    );
    let path = directory.join(name);
    crate::logging::open_no_follow(
        &path,
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600),
    )
    .map_err(|e| format!("Could not reserve database storage: {e}"))?;
    Ok(path)
}

pub(crate) fn commit(
    directory: &Path,
    path: &Path,
    profile: DatabaseProfile,
    write: impl FnOnce(&Path, &[u8]) -> std::io::Result<()>,
) -> Result<(), String> {
    let file = path
        .file_name()
        .and_then(|s| s.to_str())
        .filter(|s| valid_generation(s))
        .ok_or("Invalid staged database path.")?
        .to_string();
    let previous = read_manifest(directory);
    let mut country = previous.as_ref().and_then(|m| {
        m.country
            .clone()
            .or_else(|| (m.profile == DatabaseProfile::Country).then(|| m.file.clone()))
    });
    let mut city = previous.as_ref().and_then(|m| {
        m.city
            .clone()
            .or_else(|| (m.profile == DatabaseProfile::City).then(|| m.file.clone()))
    });
    match profile {
        DatabaseProfile::Country => country = Some(file.clone()),
        DatabaseProfile::City => city = Some(file.clone()),
    }
    let manifest = serde_json::to_vec(&Manifest {
        profile,
        file,
        country,
        city,
    })
    .map_err(|e| e.to_string())?;
    let target = directory.join("geo-dataset");
    if let Err(error) = write(&target, &manifest) {
        // write_private can fail at directory sync after its atomic rename.
        // A visible committed manifest means installation succeeded; reporting
        // failure here would falsely promise the old selection was preserved.
        if read_bounded(&target).as_deref() != Some(manifest.as_slice()) {
            return Err(format!(
                "Could not select the imported database; previous dataset preserved: {error}"
            ));
        }
        tracing::warn!(%error, "geo: dataset selected, but directory durability could not be confirmed");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn manifest_paths_are_confined_and_oversized_selection_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("geo-dataset");
        for file in [
            "../outside.mmdb",
            "/tmp/outside.mmdb",
            "GeoLite2-Country.mmdb",
            "geo-import-aa.mmdb",
        ] {
            std::fs::write(
                &path,
                serde_json::to_vec(&Manifest {
                    profile: DatabaseProfile::City,
                    file: file.into(),
                    country: None,
                    city: None,
                })
                .unwrap(),
            )
            .unwrap();
            assert!(matches!(read(dir.path()), Selection::Automatic));
        }
        std::fs::write(path, vec![b'x'; 1025]).unwrap();
        assert!(matches!(read(dir.path()), Selection::Automatic));
    }

    #[test]
    fn city_selection_retains_latest_imported_country_generation() {
        let dir = tempfile::tempdir().unwrap();
        let country = reserve(dir.path()).unwrap();
        std::fs::write(&country, b"country generation").unwrap();
        commit(
            dir.path(),
            &country,
            DatabaseProfile::Country,
            crate::private_files::write_private,
        )
        .unwrap();
        let city = reserve(dir.path()).unwrap();
        std::fs::write(&city, b"city generation").unwrap();
        commit(
            dir.path(),
            &city,
            DatabaseProfile::City,
            crate::private_files::write_private,
        )
        .unwrap();
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.path().join("geo-dataset")).unwrap())
                .unwrap();
        assert_eq!(
            manifest["country"].as_str(),
            country.file_name().unwrap().to_str()
        );
        assert_eq!(
            manifest["city"].as_str(),
            city.file_name().unwrap().to_str()
        );
        assert_eq!(manifest["profile"], "city");
        let next_country = reserve(dir.path()).unwrap();
        std::fs::write(&next_country, b"new country generation").unwrap();
        commit(
            dir.path(),
            &next_country,
            DatabaseProfile::Country,
            crate::private_files::write_private,
        )
        .unwrap();
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.path().join("geo-dataset")).unwrap())
                .unwrap();
        assert_eq!(manifest["profile"], "country");
        assert_eq!(
            manifest["country"].as_str(),
            next_country.file_name().unwrap().to_str()
        );
        assert_eq!(
            manifest["city"].as_str(),
            city.file_name().unwrap().to_str()
        );
        assert!(country.exists() && city.exists());
    }
}

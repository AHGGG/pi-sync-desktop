//! Local nonsecret profiles only. Authentication answers never enter this schema.
use crate::connection::ConnectionConfig;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub connection: ConnectionConfig,
    pub agent_dir: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredProfiles {
    version: u32,
    profiles: Vec<Profile>,
}
pub struct ProfileStore {
    path: PathBuf,
}
impl ProfileStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
    pub fn default_path() -> Result<PathBuf> {
        Ok(
            directories::ProjectDirs::from("dev", "pi-sync", "pi-sync-desktop")
                .context("Cannot locate user configuration directory")?
                .config_dir()
                .join("profiles.json"),
        )
    }
    pub fn load(&self) -> Result<Vec<Profile>> {
        let metadata = match fs::symlink_metadata(&self.path) {
            Ok(v) => v,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(e.into()),
        };
        ensure!(
            metadata.is_file()
                && !metadata.file_type().is_symlink()
                && metadata.len() <= 1024 * 1024,
            "Invalid profiles file"
        );
        let stored: StoredProfiles =
            serde_json::from_slice(&fs::read(&self.path)?).context("Invalid profiles JSON")?;
        ensure!(stored.version == 1, "Unsupported profiles version");
        validate_profiles(&stored.profiles)?;
        Ok(stored.profiles)
    }
    pub fn save(&self, profiles: &[Profile]) -> Result<()> {
        validate_profiles(profiles)?;
        let bytes = serde_json::to_vec_pretty(&StoredProfiles {
            version: 1,
            profiles: profiles.to_vec(),
        })?;
        ensure!(bytes.len() <= 1024 * 1024, "Profiles exceed size limit");
        write_private_atomic(&self.path, &bytes)
    }
}
fn validate_profiles(profiles: &[Profile]) -> Result<()> {
    ensure!(profiles.len() <= 100, "Too many profiles");
    let mut ids = std::collections::HashSet::new();
    for p in profiles {
        ensure!(
            !p.id.is_empty()
                && p.id.len() <= 60
                && p.id
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
                && ids.insert(&p.id),
            "Invalid or duplicate profile ID"
        );
        ensure!(
            !p.name.trim().is_empty()
                && p.name.len() <= 120
                && !p.name.chars().any(char::is_control),
            "Invalid profile name"
        );
        ensure!(
            !p.agent_dir.is_empty()
                && p.agent_dir.len() <= 4096
                && !p.agent_dir.chars().any(char::is_control),
            "Invalid agent directory"
        );
        p.connection.validate()?;
    }
    Ok(())
}
pub(crate) fn write_private_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("File needs a parent directory")?;
    fs::create_dir_all(parent)?;
    ensure!(
        !fs::symlink_metadata(parent)?.file_type().is_symlink(),
        "Refusing a symbolic-link configuration directory"
    );
    if let Ok(meta) = fs::symlink_metadata(path) {
        ensure!(
            meta.is_file() && !meta.file_type().is_symlink(),
            "Refusing a symbolic-link configuration file"
        );
    }
    let temporary = parent.join(format!(
        ".pi-sync-{}-{}.tmp",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| -> Result<()> {
        let mut file = options.open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        #[cfg(unix)]
        fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}
#[cfg(test)]
mod tests {
    use super::*;
    fn profile() -> Profile {
        Profile {
            id: "pi".into(),
            name: "My Pi".into(),
            connection: ConnectionConfig::parse("ssh pi@host -p 2222").unwrap(),
            agent_dir: "~/.pi/agent".into(),
        }
    }
    #[test]
    fn round_trip_and_replace_profiles() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profiles.json");
        let store = ProfileStore::new(&path);
        assert!(store.load().unwrap().is_empty());
        store.save(&[profile()]).unwrap();
        assert_eq!(store.load().unwrap(), vec![profile()]);
        store.save(&[]).unwrap();
        assert!(store.load().unwrap().is_empty());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
    #[test]
    fn rejects_secret_fields_and_duplicates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profiles.json");
        let store = ProfileStore::new(&path);
        assert!(store.save(&[profile(), profile()]).is_err());
        fs::write(
            path,
            r#"{"version":1,"profiles":[],"password":"never-save"}"#,
        )
        .unwrap();
        assert!(store.load().is_err());
    }
    #[cfg(unix)]
    #[test]
    fn refuses_symlink_store() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        fs::write(&target, "keep").unwrap();
        let path = dir.path().join("profiles.json");
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(ProfileStore::new(&path).save(&[]).is_err());
        assert_eq!(fs::read_to_string(target).unwrap(), "keep");
    }
}

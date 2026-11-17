//! Persistent browser profiles: settings, cookies and cache on disk.

use std::path::{Path, PathBuf};

use crate::settings::Settings;

/// Errors raised by profile storage.
#[derive(Debug)]
pub enum ProfileError {
    /// Wrapped I/O error.
    Io(std::io::Error),
    /// The profile directory could not be resolved.
    NoHome,
}

impl std::fmt::Display for ProfileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(err) => write!(f, "profile I/O error: {err}"),
            Self::NoHome => f.write_str("could not determine a home directory"),
        }
    }
}

impl std::error::Error for ProfileError {}

impl From<std::io::Error> for ProfileError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

/// A named profile stored under a directory.
#[derive(Debug, Clone)]
pub struct Profile {
    name: String,
    root: PathBuf,
    settings: Settings,
}

impl Profile {
    /// Creates a profile rooted at `root`, without touching the filesystem.
    pub fn at(root: impl Into<PathBuf>, name: &str) -> Self {
        Self {
            name: name.to_string(),
            root: root.into(),
            settings: Settings::default(),
        }
    }

    /// Creates (or opens) the default profile under the user's home directory.
    pub fn default_profile() -> Result<Self, ProfileError> {
        let home = std::env::var_os("HOME").ok_or(ProfileError::NoHome)?;
        Ok(Self::at(
            PathBuf::from(home)
                .join(".mag")
                .join("profiles")
                .join("default"),
            "default",
        ))
    }

    /// Profile name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Profile root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Directory holding cached responses.
    pub fn cache_dir(&self) -> PathBuf {
        self.root.join("cache")
    }

    /// Directory holding downloaded files.
    pub fn downloads_dir(&self) -> PathBuf {
        self.root.join("downloads")
    }

    /// Path of the persisted settings file.
    pub fn settings_path(&self) -> PathBuf {
        self.root.join("settings.txt")
    }

    /// Path of the persisted cookie jar.
    pub fn cookies_path(&self) -> PathBuf {
        self.root.join("cookies.txt")
    }

    /// The in-memory settings.
    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// Mutable settings.
    pub fn settings_mut(&mut self) -> &mut Settings {
        &mut self.settings
    }

    /// Creates the profile directory structure.
    pub fn ensure_dirs(&self) -> Result<(), ProfileError> {
        std::fs::create_dir_all(&self.root)?;
        std::fs::create_dir_all(self.cache_dir())?;
        std::fs::create_dir_all(self.downloads_dir())?;
        Ok(())
    }

    /// Persists settings as `key=value` lines.
    pub fn save(&self) -> Result<(), ProfileError> {
        self.ensure_dirs()?;
        let mut out = String::new();
        out.push_str(&format!(
            "viewport_width={}\n",
            self.settings.viewport_width
        ));
        out.push_str(&format!(
            "viewport_height={}\n",
            self.settings.viewport_height
        ));
        out.push_str(&format!(
            "enable_scripting={}\n",
            self.settings.enable_scripting
        ));
        out.push_str(&format!(
            "enable_network={}\n",
            self.settings.enable_network
        ));
        out.push_str(&format!(
            "prefers_color_scheme={}\n",
            self.settings.prefers_color_scheme
        ));
        out.push_str(&format!("user_agent={}\n", self.settings.user_agent));
        std::fs::write(self.settings_path(), out)?;
        Ok(())
    }

    /// Loads settings, falling back to defaults for missing keys.
    pub fn load(&mut self) -> Result<(), ProfileError> {
        let path = self.settings_path();
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Ok(());
        };
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            match key.trim() {
                "viewport_width" => {
                    self.settings.viewport_width = value.trim().parse().unwrap_or(1024.0)
                }
                "viewport_height" => {
                    self.settings.viewport_height = value.trim().parse().unwrap_or(768.0)
                }
                "enable_scripting" => self.settings.enable_scripting = value.trim() == "true",
                "enable_network" => self.settings.enable_network = value.trim() == "true",
                "prefers_color_scheme" => {
                    self.settings.prefers_color_scheme = value.trim().to_string();
                }
                "user_agent" => self.settings.user_agent = value.trim().to_string(),
                _ => {}
            }
        }
        Ok(())
    }

    /// Appends a persisted cookie line.
    pub fn append_cookie(&self, domain: &str, name: &str, value: &str) -> Result<(), ProfileError> {
        self.ensure_dirs()?;
        let mut line = String::new();
        if let Ok(existing) = std::fs::read_to_string(self.cookies_path()) {
            line.push_str(&existing);
        }
        line.push_str(&format!("{domain}\t{name}\t{value}\n"));
        std::fs::write(self.cookies_path(), line)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saves_and_loads_settings() {
        let dir = std::env::temp_dir().join(format!("mag-profile-{}", std::process::id()));
        let mut profile = Profile::at(&dir, "test");
        profile.settings_mut().viewport_width = 999.0;
        profile.settings_mut().enable_scripting = true;
        profile.save().unwrap();
        assert!(profile.settings_path().exists());
        let mut loaded = Profile::at(&dir, "test");
        loaded.load().unwrap();
        assert_eq!(loaded.settings().viewport_width, 999.0);
        assert!(loaded.settings().enable_scripting);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_settings_file_is_not_an_error() {
        let mut profile = Profile::at(std::env::temp_dir().join("mag-profile-missing"), "t");
        assert!(profile.load().is_ok());
    }
}

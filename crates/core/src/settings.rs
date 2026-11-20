//! The user's settings, the same in every shell: kept in a small text file (one
//! `key = value` a line) beside the recent list, in the folder the shell names.
//! Keys this version doesn't know are kept as they were, so an older build doesn't
//! lose a newer one's settings; values it can't read fall back to the default.

use std::path::{Path, PathBuf};

/// Light or dark.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Theme {
    /// As the system is.
    #[default]
    System,
    Light,
    Dark,
}

impl Theme {
    /// Dark or not, given whether the system is.
    pub fn dark(self, system_dark: bool) -> bool {
        match self {
            Self::System => system_dark,
            Self::Light => false,
            Self::Dark => true,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    fn from_name(s: &str) -> Option<Self> {
        Some(match s {
            "system" => Self::System,
            "light" => Self::Light,
            "dark" => Self::Dark,
            _ => return None,
        })
    }
}

/// Which interface an app with two draws (the Apple apps): its own platform's, or
/// libgui's, the libgui app's interface in the same window. The libgui app has only
/// libgui's and ignores it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Interface {
    #[default]
    Native,
    Libgui,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub theme: Theme,
    /// At launch with no file named, open the last file again (at its place).
    pub reopen_last: bool,
    pub interface: Interface,
}

impl Default for Settings {
    fn default() -> Self {
        Self { theme: Theme::System, reopen_last: true, interface: Interface::Native }
    }
}

/// The settings, and the file they're kept in.
#[derive(Debug, Default)]
pub struct SettingsFile {
    file: Option<PathBuf>,
    values: Settings,
    /// Lines with keys this version doesn't know, kept for the next save.
    unknown: Vec<String>,
    dirty: bool,
}

impl SettingsFile {
    /// Only in memory: nothing is saved.
    pub fn in_memory() -> Self {
        Self::default()
    }

    /// The settings kept in `file`; the defaults when it's missing or unreadable.
    pub fn load(file: PathBuf) -> Self {
        let mut values = Settings::default();
        let mut unknown = Vec::new();
        for line in std::fs::read_to_string(&file).unwrap_or_default().lines() {
            let Some((key, value)) = line.split_once('=') else { continue };
            match (key.trim(), value.trim()) {
                ("theme", v) => values.theme = Theme::from_name(v).unwrap_or_default(),
                ("reopen_last", v) => values.reopen_last = v != "false",
                ("interface", v) => values.interface = if v == "libgui" { Interface::Libgui } else { Interface::Native },
                _ => unknown.push(line.to_owned()),
            }
        }
        Self { file: Some(file), values, unknown, dirty: false }
    }

    pub fn get(&self) -> &Settings {
        &self.values
    }

    pub fn set(&mut self, values: Settings) {
        self.dirty |= values != self.values;
        self.values = values;
    }

    /// Writes the file if anything changed: to a new file put in place of the old.
    pub fn save(&mut self) -> std::io::Result<()> {
        let Some(file) = &self.file else { return Ok(()) };
        if !self.dirty {
            return Ok(());
        }
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let interface = match self.values.interface {
            Interface::Native => "native",
            Interface::Libgui => "libgui",
        };
        let mut text = format!("theme = {}\nreopen_last = {}\ninterface = {interface}\n", self.values.theme.name(), self.values.reopen_last);
        for line in &self.unknown {
            text.push_str(line);
            text.push('\n');
        }
        write_atomically(file, &text)?;
        self.dirty = false;
        Ok(())
    }
}

pub(crate) fn write_atomically(file: &Path, text: &str) -> std::io::Result<()> {
    let tmp = file.with_extension("tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_and_loaded_keeping_what_it_doesnt_know() {
        let dir = std::env::temp_dir().join(format!("viewer-settings-{}", std::process::id()));
        let file = dir.join("settings.txt");
        let mut s = SettingsFile::load(file.clone());
        assert_eq!(s.get(), &Settings::default());
        s.set(Settings { theme: Theme::Dark, reopen_last: false, interface: Interface::Libgui });
        s.save().unwrap();
        // A newer build's setting, and a value this one can't read.
        let text = std::fs::read_to_string(&file).unwrap();
        std::fs::write(&file, format!("{text}future = 7\n")).unwrap();
        let mut back = SettingsFile::load(file.clone());
        assert_eq!(back.get(), &Settings { theme: Theme::Dark, reopen_last: false, interface: Interface::Libgui });
        back.set(Settings { theme: Theme::Light, reopen_last: false, interface: Interface::Native });
        back.save().unwrap();
        assert!(std::fs::read_to_string(&file).unwrap().contains("future = 7"));
        std::fs::write(&file, "theme = purple\n").unwrap();
        assert_eq!(SettingsFile::load(file).get().theme, Theme::System);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn theme_against_the_system() {
        assert!(Theme::System.dark(true) && !Theme::System.dark(false));
        assert!(Theme::Dark.dark(false) && !Theme::Light.dark(true));
    }
}

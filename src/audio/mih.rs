//! MIH asset discovery. Only MIH-specific directories are searched, so another
//! effect's activation/deactivation MP3 can never be borrowed as a fallback.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MihSound {
    Activation,
    Acceleration,
    Maximum,
    UniverseReset,
    Deactivation,
}

impl MihSound {
    pub const ALL: [Self; 5] = [
        Self::Activation,
        Self::Acceleration,
        Self::Maximum,
        Self::UniverseReset,
        Self::Deactivation,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Activation => "Activation",
            Self::Acceleration => "Acceleration",
            Self::Maximum => "Maximum acceleration",
            Self::UniverseReset => "Universe Reset",
            Self::Deactivation => "Deactivation",
        }
    }

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|s| *s == self).unwrap_or(0)
    }

    pub fn from_index(index: i32) -> Self {
        usize::try_from(index)
            .ok()
            .and_then(|i| Self::ALL.get(i).copied())
            .unwrap_or(Self::Activation)
    }
}

pub fn classify(path: &Path) -> Option<MihSound> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    if extension != "mp3" && extension != "wav" {
        return None;
    }
    let name: String = path
        .file_stem()?
        .to_str()?
        .to_ascii_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    // Order matters: reset / deactivation / maximum must win over "activation".
    if name.contains("universereset") || name == "reset" {
        Some(MihSound::UniverseReset)
    } else if name.contains("deactivat")
        || name.contains("powerdown")
        || name.ends_with("end")
        || name == "stop"
    {
        Some(MihSound::Deactivation)
    } else if name.contains("maximum") || name.contains("maxaccel") || name.contains("ascended") {
        Some(MihSound::Maximum)
    } else if name.contains("accelerat") {
        Some(MihSound::Acceleration)
    } else if name.contains("summon") || name.contains("activat") || name.ends_with("start") {
        Some(MihSound::Activation)
    } else {
        None
    }
}

/// User settings folder first, then portable / cargo layouts. The actual supplied
/// folder is MIHassets; filenames are classified rather than guessed or renamed.
pub fn asset_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for base in super::engine::sound_dirs() {
        dirs.push(base.join("mih"));
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        dirs.push(dir.join("MIHassets"));
        dirs.push(dir.join("assets").join("MIHassets"));
        if let Some(root) = dir.parent().and_then(|d| d.parent()) {
            dirs.push(root.join("MIHassets"));
        }
    }
    dirs
}

pub fn discover(dirs: &[PathBuf]) -> HashMap<MihSound, PathBuf> {
    let mut files = HashMap::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut paths: Vec<_> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_file())
            .collect();
        paths.sort(); // deterministic even if a directory contains multiple takes
        for path in paths {
            if let Some(event) = classify(&path) {
                files.entry(event).or_insert(path);
            }
        }
    }
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_the_actual_supplied_mp3_names_without_substitution() {
        for (name, event) in [
            ("made-in-heaven-summon-start.mp3", MihSound::Activation),
            ("time-accelerate.mp3", MihSound::Acceleration),
            ("universe-reset.mp3", MihSound::UniverseReset),
            ("mih-maximum-acceleration.mp3", MihSound::Maximum),
            ("mih-deactivation.mp3", MihSound::Deactivation),
        ] {
            assert_eq!(classify(Path::new(name)), Some(event));
        }
        assert_eq!(classify(Path::new("sandevistan.mp3")), None);
        assert_eq!(classify(Path::new("dio_stop.mp3")), None);
        assert_eq!(classify(Path::new("made-in-heaven-summon-start.txt")), None);
    }

    #[test]
    fn missing_directories_are_silent() {
        assert!(discover(&[PathBuf::from("a-nonexistent-mih-test-folder")]).is_empty());
    }
}

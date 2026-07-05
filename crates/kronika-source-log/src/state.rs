//! Persistent tail offset state.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::parser::ParserKind;

/// Persisted position of a tailed `PostgreSQL` log file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TailState {
    /// Path of the tailed file.
    pub path: PathBuf,
    /// Device id of the tailed file.
    pub dev: u64,
    /// Inode of the tailed file.
    pub inode: u64,
    /// Byte offset to resume from.
    pub offset: u64,
    /// Parser kind used for this file.
    pub parser_kind: ParserKind,
    /// Whether resume must skip bytes until the next newline.
    pub skip_until_newline: bool,
}

impl TailState {
    /// Read a state file if it exists.
    ///
    /// # Errors
    ///
    /// Returns I/O errors other than `NotFound`; malformed files are ignored.
    pub fn load(path: &Path) -> io::Result<Option<Self>> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err),
        };
        Ok(parse_state(&text))
    }

    /// Persist the state atomically enough for collector restart recovery.
    ///
    /// # Errors
    ///
    /// Returns filesystem errors while creating the directory, writing the temp
    /// file, or renaming it over the previous state.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("tmp");
        fs::write(&tmp, self.render())?;
        fs::rename(tmp, path)
    }

    fn render(&self) -> String {
        format!(
            "version=1\npath={}\ndev={}\ninode={}\noffset={}\nparser={}\nskip_until_newline={}\n",
            self.path.display(),
            self.dev,
            self.inode,
            self.offset,
            self.parser_kind.as_state_value(),
            self.skip_until_newline
        )
    }
}

fn parse_state(text: &str) -> Option<TailState> {
    let mut path = None;
    let mut dev = None;
    let mut inode = None;
    let mut offset = None;
    let mut parser_kind = None;
    let mut skip_until_newline = false;
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key {
            "path" => path = Some(PathBuf::from(value)),
            "dev" => dev = value.parse().ok(),
            "inode" => inode = value.parse().ok(),
            "offset" => offset = value.parse().ok(),
            "parser" => parser_kind = ParserKind::parse(value),
            "skip_until_newline" => skip_until_newline = value == "true",
            _ => {}
        }
    }
    Some(TailState {
        path: path?,
        dev: dev?,
        inode: inode?,
        offset: offset?,
        parser_kind: parser_kind?,
        skip_until_newline,
    })
}

#[cfg(test)]
mod tests {
    use super::TailState;
    use crate::ParserKind;

    #[test]
    fn state_roundtrip_preserves_resume_fields() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state.txt");
        let state = TailState {
            path: dir.path().join("postgresql.log"),
            dev: 10,
            inode: 20,
            offset: 30,
            parser_kind: ParserKind::Stderr,
            skip_until_newline: true,
        };
        state.save(&path).expect("save");
        assert_eq!(TailState::load(&path).expect("load"), Some(state));
    }

    #[test]
    fn malformed_state_is_ignored() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state.txt");
        std::fs::write(&path, "path=/tmp/log\n").expect("write");
        assert_eq!(TailState::load(&path).expect("load"), None);
    }
}

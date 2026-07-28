//! Read-only inspection of a `PgKronika` data tree, PGM segment, or active journal.

mod journal;
mod model;
mod ovf;
mod pgm;
mod tree;

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use rustix::fs::{Mode, OFlags};

use crate::model::Output;

#[cfg(test)]
use kronika_writer as _;
#[cfg(test)]
use tempfile as _;

const USAGE: &str = "usage: pg_kronika-dump <path> [--rows] [--limit N]";
const DEFAULT_ROW_LIMIT: usize = 1_000;

#[derive(Debug, Clone, Copy)]
pub(crate) struct Options {
    pub(crate) rows: bool,
    pub(crate) limit: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ErrorKind {
    Input,
    Usage,
}

/// A usage or input-data failure reported by the dump command.
#[derive(Debug)]
pub(crate) struct DumpError {
    kind: ErrorKind,
    message: String,
}

impl DumpError {
    pub(crate) fn input(context: &'static str, error: impl fmt::Display) -> Self {
        Self {
            kind: ErrorKind::Input,
            message: format!("{context}: {error}"),
        }
    }

    pub(crate) fn message(message: impl Into<String>) -> Self {
        Self {
            kind: ErrorKind::Input,
            message: message.into(),
        }
    }

    fn usage(message: impl Into<String>) -> Self {
        Self {
            kind: ErrorKind::Usage,
            message: message.into(),
        }
    }
}

impl fmt::Display for DumpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for DumpError {}

/// Runs the command with injectable arguments and streams.
///
/// The command writes exactly one JSON object to `stdout` on success. Usage and
/// input diagnostics are written to `stderr`.
pub fn run<I, W, E>(args: I, mut stdout: W, mut stderr: E) -> ExitCode
where
    I: IntoIterator<Item = OsString>,
    W: io::Write,
    E: io::Write,
{
    match execute(args) {
        Ok(output) => {
            if let Err(error) = write_output(&mut stdout, &output) {
                let _ignored = writeln!(stderr, "pg_kronika-dump: write stdout: {error}");
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(error) => {
            let _ignored = writeln!(stderr, "pg_kronika-dump: {error}");
            if error.kind == ErrorKind::Usage {
                let _ignored = writeln!(stderr, "{USAGE}");
                ExitCode::from(2)
            } else {
                ExitCode::from(1)
            }
        }
    }
}

fn execute<I>(args: I) -> Result<Output, DumpError>
where
    I: IntoIterator<Item = OsString>,
{
    let arguments = parse_args(args)?;
    let metadata = std::fs::symlink_metadata(&arguments.path)
        .map_err(|error| DumpError::input("inspect input path", error))?;
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        return Err(DumpError::message("input path must not be a symbolic link"));
    }
    if file_type.is_dir() {
        if arguments.options.rows {
            return Err(DumpError::usage(
                "--rows cannot be used when inspecting a data directory",
            ));
        }
        return tree::inspect_path(&arguments.path, arguments.options).map(Output::Tree);
    }
    if !file_type.is_file() {
        return Err(DumpError::message(
            "input path is neither a regular file nor a directory",
        ));
    }
    let file = open_regular_input(&arguments.path)?;
    match arguments.path.extension() {
        Some(extension) if extension == OsStr::new("pgm") => {
            pgm::inspect_file(file, &arguments.path, arguments.options).map(Output::Pgm)
        }
        Some(extension) if extension == OsStr::new("ovf") => {
            ovf::inspect_file(file, &arguments.path, arguments.options).map(Output::Ovf)
        }
        _ => journal::inspect_file(&file, &arguments.path, arguments.options).map(Output::Journal),
    }
}

fn write_output(writer: &mut impl io::Write, output: &Output) -> io::Result<()> {
    serde_json::to_writer_pretty(&mut *writer, output).map_err(io::Error::other)?;
    writer.write_all(b"\n")
}

fn open_regular_input(path: &Path) -> Result<File, DumpError> {
    let file = rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(|error| {
        DumpError::input(
            "open input file",
            io::Error::from_raw_os_error(error.raw_os_error()),
        )
    })?;
    if file
        .metadata()
        .map_err(|error| DumpError::input("stat input file", error))?
        .is_file()
    {
        Ok(file)
    } else {
        Err(DumpError::message("input path is not a regular file"))
    }
}

struct Arguments {
    path: PathBuf,
    options: Options,
}

fn parse_args<I>(args: I) -> Result<Arguments, DumpError>
where
    I: IntoIterator<Item = OsString>,
{
    let mut iterator = args.into_iter();
    let mut path = None;
    let mut rows = false;
    let mut limit = None;
    let mut positional_only = false;

    while let Some(argument) = iterator.next() {
        if !positional_only && argument == OsStr::new("--") {
            positional_only = true;
            continue;
        }
        if !positional_only && argument == OsStr::new("--rows") {
            if rows {
                return Err(DumpError::usage("--rows was specified more than once"));
            }
            rows = true;
            continue;
        }
        if !positional_only && argument == OsStr::new("--limit") {
            if limit.is_some() {
                return Err(DumpError::usage("--limit was specified more than once"));
            }
            let value = iterator
                .next()
                .ok_or_else(|| DumpError::usage("--limit requires an integer value"))?;
            limit = Some(parse_limit(&value)?);
            continue;
        }
        if !positional_only
            && let Some(value) = argument
                .to_str()
                .and_then(|argument| argument.strip_prefix("--limit="))
        {
            if limit.is_some() {
                return Err(DumpError::usage("--limit was specified more than once"));
            }
            limit = Some(parse_limit(OsStr::new(value))?);
            continue;
        }
        if !positional_only && argument.to_string_lossy().starts_with('-') {
            return Err(DumpError::usage(format!(
                "unknown option: {}",
                argument.to_string_lossy()
            )));
        }
        if path.replace(PathBuf::from(argument)).is_some() {
            return Err(DumpError::usage("exactly one input path is required"));
        }
    }

    let path = path.ok_or_else(|| DumpError::usage("input path is required"))?;
    if limit.is_some() && !rows {
        return Err(DumpError::usage("--limit requires --rows"));
    }
    Ok(Arguments {
        path,
        options: Options {
            rows,
            limit: limit.unwrap_or(DEFAULT_ROW_LIMIT),
        },
    })
}

fn parse_limit(value: &OsStr) -> Result<usize, DumpError> {
    let value = value
        .to_str()
        .ok_or_else(|| DumpError::usage("--limit must be valid UTF-8"))?;
    value
        .parse()
        .map_err(|_error| DumpError::usage("--limit must be a non-negative integer"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_require_limit_to_be_tied_to_rows() {
        let error = parse_args([OsString::from("file.pgm"), OsString::from("--limit=1")])
            .err()
            .expect("arguments must fail");
        assert_eq!(error.kind, ErrorKind::Usage);
        assert_eq!(error.to_string(), "--limit requires --rows");
    }

    #[test]
    fn arguments_accept_non_utf8_paths() {
        use std::os::unix::ffi::OsStringExt as _;

        let path = OsString::from_vec(vec![b'd', b'a', b't', b'a', 0xff]);
        let parsed = parse_args([path.clone(), OsString::from("--rows")]).unwrap();
        assert_eq!(parsed.path, PathBuf::from(path));
        assert!(parsed.options.rows);
        assert_eq!(parsed.options.limit, DEFAULT_ROW_LIMIT);
    }
}

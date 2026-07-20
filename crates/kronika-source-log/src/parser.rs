//! Stderr parser for the first log-domain scope.

use crate::{MAX_TEXT_BYTES, truncate_utf8};

/// Parser kind selected for a log source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParserKind {
    /// `PostgreSQL` `stderr` text log.
    Stderr,
    /// `PostgreSQL` `csvlog`; reserved until the byte-level CSV parser lands.
    Csvlog,
    /// No known parser was selected.
    Unknown,
}

impl ParserKind {
    /// Numeric code stored in `pg_log_gap`.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::Stderr => 0,
            Self::Csvlog => 1,
            Self::Unknown => 2,
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "stderr" => Some(Self::Stderr),
            "csvlog" => Some(Self::Csvlog),
            "unknown" => Some(Self::Unknown),
            _ => None,
        }
    }

    pub(crate) const fn as_state_value(self) -> &'static str {
        match self {
            Self::Stderr => "stderr",
            Self::Csvlog => "csvlog",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ParsedLine<'a> {
    Error {
        ts: Option<i64>,
        severity: LogSeverity,
        sqlstate: Option<&'a str>,
        message: &'a str,
    },
    Continuation {
        kind: ContinuationKind,
        text: &'a str,
    },
}

/// Structured stderr continuation payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContinuationKind {
    Detail,
    Hint,
    Context,
    Statement,
}

/// Severity stored in grouped log errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LogSeverity {
    /// `ERROR`.
    Error,
    /// `FATAL`.
    Fatal,
    /// `PANIC`.
    Panic,
    /// `WARNING`.
    Warning,
    /// Selected lifecycle `LOG` records that carry crash/OOM signal data.
    Log,
}

impl LogSeverity {
    /// Numeric code stored in `pg_log_errors`.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::Error => 0,
            Self::Fatal => 1,
            Self::Panic => 2,
            Self::Warning => 3,
            Self::Log => 4,
        }
    }
}

const SEVERITIES: &[(&str, LogSeverity)] = &[
    ("PANIC:  ", LogSeverity::Panic),
    ("FATAL:  ", LogSeverity::Fatal),
    ("ERROR:  ", LogSeverity::Error),
    ("WARNING:  ", LogSeverity::Warning),
    ("LOG:  ", LogSeverity::Log),
    ("ПАНИКА:  ", LogSeverity::Panic),
    ("ВАЖНО:  ", LogSeverity::Fatal),
    ("ОШИБКА:  ", LogSeverity::Error),
    ("ПРЕДУПРЕЖДЕНИЕ:  ", LogSeverity::Warning),
    ("СООБЩЕНИЕ:  ", LogSeverity::Log),
];

const DETAIL_PREFIXES: &[&str] = &["DETAIL:  ", "ПОДРОБНОСТИ:  "];
const HINT_PREFIXES: &[&str] = &["HINT:  ", "ПОДСКАЗКА:  "];
const CONTEXT_PREFIXES: &[&str] = &["CONTEXT:  ", "КОНТЕКСТ:  "];
const STATEMENT_PREFIXES: &[&str] = &["STATEMENT:  ", "ОПЕРАТОР:  "];

pub(crate) fn parse_stderr_line(line: &str) -> Option<ParsedLine<'_>> {
    let line = line.strip_suffix('\r').unwrap_or(line);
    let severity = SEVERITIES
        .iter()
        .filter_map(|&(keyword, severity)| line.find(keyword).map(|pos| (pos, keyword, severity)))
        .min_by_key(|(pos, _, _)| *pos);
    if let Some((pos, keyword, severity)) = severity {
        let message = line.get(pos + keyword.len()..)?.trim();
        if message.is_empty() {
            return None;
        }
        let (sqlstate, message) = strip_sqlstate(message);
        return Some(ParsedLine::Error {
            ts: parse_prefix_ts(line),
            severity,
            sqlstate,
            message,
        });
    }

    for (kind, prefixes) in [
        (ContinuationKind::Detail, DETAIL_PREFIXES),
        (ContinuationKind::Hint, HINT_PREFIXES),
        (ContinuationKind::Context, CONTEXT_PREFIXES),
        (ContinuationKind::Statement, STATEMENT_PREFIXES),
    ] {
        for prefix in prefixes {
            if let Some(parsed) = parse_continuation(line, prefix, kind) {
                return Some(parsed);
            }
        }
    }

    None
}

fn parse_continuation<'a>(
    line: &'a str,
    prefix: &str,
    kind: ContinuationKind,
) -> Option<ParsedLine<'a>> {
    if let Some(pos) = line.find(prefix) {
        let text = line.get(pos + prefix.len()..)?.trim();
        return Some(ParsedLine::Continuation {
            kind,
            text: truncate_utf8(text, MAX_TEXT_BYTES),
        });
    }
    None
}

pub(crate) fn strip_sqlstate(message: &str) -> (Option<&str>, &str) {
    let bytes = message.as_bytes();
    if bytes.len() > 7
        && bytes[..5]
            .iter()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
        && bytes[5] == b':'
        && bytes[6] == b' '
        && bytes[7] == b' '
    {
        return (
            message.get(..5),
            message.get(8..).unwrap_or_default().trim(),
        );
    }
    (None, message)
}

fn parse_prefix_ts(line: &str) -> Option<i64> {
    let bytes = line.as_bytes();
    if bytes.len() < 19 {
        return None;
    }
    if !matches!(
        (bytes[4], bytes[7], bytes[10], bytes[13], bytes[16]),
        (b'-', b'-', b' ', b':', b':')
    ) {
        return None;
    }
    let year = parse_digits(line.get(0..4)?)?;
    let month = parse_digits(line.get(5..7)?)?;
    let day = parse_digits(line.get(8..10)?)?;
    let hour = parse_digits(line.get(11..13)?)?;
    let minute = parse_digits(line.get(14..16)?)?;
    let second = parse_digits(line.get(17..19)?)?;
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    let micros = parse_fractional_micros(line.get(19..).unwrap_or_default());
    let days = days_from_civil(year, month, day);
    let seconds = days
        .checked_mul(86_400)?
        .checked_add(i64::from(hour) * 3600)?
        .checked_add(i64::from(minute) * 60)?
        .checked_add(i64::from(second))?;
    seconds
        .checked_mul(1_000_000)?
        .checked_add(i64::from(micros))
}

fn parse_digits(value: &str) -> Option<i32> {
    if value.as_bytes().iter().all(u8::is_ascii_digit) {
        value.parse().ok()
    } else {
        None
    }
}

fn parse_fractional_micros(rest: &str) -> u32 {
    let Some(rest) = rest.strip_prefix('.') else {
        return 0;
    };
    let mut micros = 0_u32;
    let mut scale = 100_000_u32;
    for b in rest.as_bytes().iter().copied().take(6) {
        if !b.is_ascii_digit() {
            break;
        }
        micros += u32::from(b - b'0') * scale;
        scale /= 10;
    }
    micros
}

fn days_from_civil(year: i32, month: i32, day: i32) -> i64 {
    let year = year - i32::from(month <= 2);
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let yoe = year - era * 400;
    let mp = month + if month > 2 { -3 } else { 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    i64::from(era) * 146_097 + i64::from(doe) - 719_468
}

#[cfg(test)]
mod tests {
    use super::{ContinuationKind, LogSeverity, ParsedLine, parse_stderr_line, strip_sqlstate};

    #[test]
    fn parses_stderr_error_and_sqlstate() {
        let parsed = parse_stderr_line(
            "2026-07-05 12:30:45.123 UTC [42]: ERROR:  42P01:  relation \"foo\" does not exist",
        )
        .expect("error parsed");
        let ParsedLine::Error {
            ts,
            severity,
            sqlstate,
            message,
        } = parsed
        else {
            panic!("expected error");
        };
        assert_eq!(severity, LogSeverity::Error);
        assert_eq!(sqlstate, Some("42P01"));
        assert_eq!(message, "relation \"foo\" does not exist");
        assert_eq!(ts, Some(1_783_254_645_123_000));
    }

    #[test]
    fn parses_russian_warning() {
        let parsed = parse_stderr_line("2026-07-05 12:30:45 UTC [42]: ПРЕДУПРЕЖДЕНИЕ:  внимание")
            .expect("warning parsed");
        assert!(matches!(
            parsed,
            ParsedLine::Error {
                severity: LogSeverity::Warning,
                message: "внимание",
                ..
            }
        ));
    }

    #[test]
    fn parses_postmaster_log_severity() {
        let parsed = parse_stderr_line(
            "2026-07-05 12:30:45 UTC [42]: LOG:  server process (PID 4242) was terminated by signal 9: Killed",
        )
        .expect("log parsed");
        assert!(matches!(
            parsed,
            ParsedLine::Error {
                severity: LogSeverity::Log,
                message: "server process (PID 4242) was terminated by signal 9: Killed",
                ..
            }
        ));
    }

    #[test]
    fn message_text_cannot_spoof_a_later_more_severe_marker() {
        let parsed = parse_stderr_line(
            "2026-07-05 12:30:45 UTC [42]: ERROR:  user text contains PANIC:  but is not panic",
        )
        .expect("error parsed");
        assert!(matches!(
            parsed,
            ParsedLine::Error {
                severity: LogSeverity::Error,
                message: "user text contains PANIC:  but is not panic",
                ..
            }
        ));
    }

    #[test]
    fn parses_statement_line() {
        let parsed =
            parse_stderr_line("2026-07-05 12:30:45 UTC [42]: STATEMENT:  select pg_sleep(10)")
                .expect("statement parsed");
        assert!(matches!(
            parsed,
            ParsedLine::Continuation {
                kind: ContinuationKind::Statement,
                text: "select pg_sleep(10)"
            }
        ));
    }

    #[test]
    fn parses_typed_continuation_lines() {
        assert_eq!(
            parse_stderr_line("2026-07-05 12:30:46 UTC [42]: DETAIL:  Process 1 waits")
                .expect("detail parsed"),
            ParsedLine::Continuation {
                kind: ContinuationKind::Detail,
                text: "Process 1 waits"
            }
        );
        assert_eq!(
            parse_stderr_line("2026-07-05 12:30:47 UTC [42]: HINT:  See server log")
                .expect("hint parsed"),
            ParsedLine::Continuation {
                kind: ContinuationKind::Hint,
                text: "See server log"
            }
        );
        assert_eq!(
            parse_stderr_line("2026-07-05 12:30:48 UTC [42]: CONTEXT:  while updating tuple")
                .expect("context parsed"),
            ParsedLine::Continuation {
                kind: ContinuationKind::Context,
                text: "while updating tuple"
            }
        );
    }

    #[test]
    fn strips_sqlstate_only_at_the_message_start() {
        assert_eq!(
            strip_sqlstate("42P01:  relation \"foo\" does not exist"),
            (Some("42P01"), "relation \"foo\" does not exist")
        );
        assert_eq!(
            strip_sqlstate("ERROR 42P01: text"),
            (None, "ERROR 42P01: text")
        );
    }
}

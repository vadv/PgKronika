//! Error normalization and category classification.

use crate::parser::LogSeverity;
use crate::{MAX_PATTERN_BYTES, truncate_utf8};

/// Error category stored in grouped log errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ErrorCategory {
    /// Lock contention.
    Lock,
    /// Constraint violations.
    Constraint,
    /// Serialization failures.
    Serialization,
    /// Timeout or cancellation.
    Timeout,
    /// Resource exhaustion.
    Resource,
    /// Data corruption or `PANIC`.
    DataCorruption,
    /// System-level failures or uncategorized `FATAL`.
    System,
    /// Connection failures.
    Connection,
    /// Authentication or authorization failures.
    Auth,
    /// SQL syntax or semantic errors.
    Syntax,
    /// Uncategorized errors.
    Other,
}

impl ErrorCategory {
    /// Numeric code stored in `pg_log_errors`.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::Lock => 0,
            Self::Constraint => 1,
            Self::Serialization => 2,
            Self::Timeout => 3,
            Self::Resource => 4,
            Self::DataCorruption => 5,
            Self::System => 6,
            Self::Connection => 7,
            Self::Auth => 8,
            Self::Syntax => 9,
            Self::Other => 10,
        }
    }
}

pub(crate) fn normalize_error(message: &str) -> String {
    let mut value = strip_at_character(message).to_owned();
    value = replace_quoted(&value, '"', "\"...\"");
    value = replace_quoted(&value, '\'', "'...'");
    value = replace_delimited(&value, '(', ')', "(...)");
    value = replace_delimited(&value, '[', ']', "[...]");
    value = replace_word_patterns(&value);
    truncate_utf8(&value, MAX_PATTERN_BYTES).to_owned()
}

#[allow(
    clippy::too_many_lines,
    reason = "the ordered taxonomy is kept in one readable table so category precedence stays auditable"
)]
pub(crate) fn classify_error(pattern: &str, severity: LogSeverity) -> ErrorCategory {
    if severity == LogSeverity::Panic {
        return ErrorCategory::DataCorruption;
    }
    let lower = pattern.to_ascii_lowercase();
    let category = if any_contains(
        &lower,
        &[
            "deadlock detected",
            "could not obtain lock",
            "lock timeout",
            "still waiting for",
        ],
    ) {
        ErrorCategory::Lock
    } else if any_contains(
        &lower,
        &[
            "duplicate key",
            "violates foreign key",
            "violates not-null",
            "violates check",
            "violates exclusion",
            "null value in column",
        ],
    ) {
        ErrorCategory::Constraint
    } else if lower.contains("could not serialize access") {
        ErrorCategory::Serialization
    } else if any_contains(
        &lower,
        &[
            "statement timeout",
            "idle-in-transaction session timeout",
            "transaction timeout",
            "idle session timeout",
            "canceling statement due to user request",
        ],
    ) {
        ErrorCategory::Timeout
    } else if any_contains(
        &lower,
        &[
            "out of memory",
            "out of shared memory",
            "too many connections",
            "disk full",
            "could not extend",
            "remaining connection slots",
        ],
    ) || is_signal_kill(&lower)
    {
        ErrorCategory::Resource
    } else if any_contains(
        &lower,
        &[
            "invalid page",
            "could not read block",
            "data corrupted",
            "index corrupted",
            "unexpected zero page",
            "invalid checkpoint record",
            "invalid memory alloc",
        ],
    ) {
        ErrorCategory::DataCorruption
    } else if any_contains(
        &lower,
        &[
            "could not open file",
            "i/o error",
            "crash shutdown",
            "server process",
            "shutting down",
        ],
    ) {
        ErrorCategory::System
    } else if any_contains(
        &lower,
        &[
            "connection reset by peer",
            "unexpected eof",
            "broken pipe",
            "could not receive data",
            "could not send data",
            "terminating connection",
        ],
    ) {
        ErrorCategory::Connection
    } else if any_contains(
        &lower,
        &[
            "password authentication failed",
            "no pg_hba.conf entry",
            "role ",
            "permission denied",
            "ssl connection is required",
        ],
    ) {
        ErrorCategory::Auth
    } else if any_contains(
        &lower,
        &[
            "syntax error",
            "does not exist",
            "invalid input syntax",
            "division by zero",
            "value too long",
        ],
    ) {
        ErrorCategory::Syntax
    } else {
        ErrorCategory::Other
    };
    if category == ErrorCategory::Other && severity == LogSeverity::Fatal {
        ErrorCategory::System
    } else {
        category
    }
}

fn any_contains(value: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| value.contains(needle))
}

fn is_signal_kill(value: &str) -> bool {
    value.contains("terminated by signal") && value.contains(": killed")
}

fn strip_at_character(value: &str) -> &str {
    value
        .find(" at character ")
        .and_then(|pos| value.get(..pos))
        .unwrap_or(value)
}

fn replace_quoted(value: &str, quote: char, replacement: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.char_indices();
    while let Some((start, c)) = chars.next() {
        if c != quote {
            out.push(c);
            continue;
        }
        let mut closed = None;
        for (idx, next) in chars.by_ref() {
            if next == quote {
                closed = Some(idx);
                break;
            }
        }
        match closed {
            Some(end) if end > start + quote.len_utf8() => out.push_str(replacement),
            Some(_) => {
                out.push(quote);
                out.push(quote);
            }
            None => out.push(quote),
        }
    }
    out
}

fn replace_delimited(value: &str, open: char, close: char, replacement: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != open {
            out.push(c);
            continue;
        }
        let mut closed = false;
        for next in chars.by_ref() {
            if next == close {
                closed = true;
                break;
            }
        }
        if closed {
            out.push_str(replacement);
        } else {
            out.push(open);
        }
    }
    out
}

fn replace_word_patterns(value: &str) -> String {
    let mut out = value.to_owned();
    for prefix in [
        "transaction ",
        "relation ",
        "process ",
        "database ",
        "PID ",
        "signal ",
        "on page ",
    ] {
        out = replace_word_number(&out, prefix);
    }
    out = replace_word_float(&out, "after ");
    out = replace_wal_address(&out);
    if let Some(pos) = out.find("invalid input syntax for ")
        && let Some(colon) = out.get(pos..).and_then(|tail| tail.find(": "))
    {
        out.truncate(pos + colon + 2);
        out.push_str("...");
    }
    for object in [
        "permission denied for table ",
        "permission denied for schema ",
        "permission denied for sequence ",
        "permission denied for function ",
        "permission denied for database ",
    ] {
        if let Some(pos) = out.to_ascii_lowercase().find(object) {
            out.truncate(pos + object.len());
            out.push_str("...");
            break;
        }
    }
    if let Some(pos) = out.find("byte sequence for encoding") {
        out.truncate(pos + "byte sequence for encoding".len());
        out.push_str(" ...");
    }
    out
}

fn replace_word_number(value: &str, prefix: &str) -> String {
    replace_after(value, prefix, |b| b.is_ascii_digit())
}

fn replace_word_float(value: &str, prefix: &str) -> String {
    replace_after(value, prefix, |b| b.is_ascii_digit() || b == b'.')
}

fn replace_after(value: &str, prefix: &str, keep: impl Fn(u8) -> bool) -> String {
    let Some(pos) = value.find(prefix) else {
        return value.to_owned();
    };
    let after = pos + prefix.len();
    let rest = &value.as_bytes()[after..];
    if !rest.first().is_some_and(|b| keep(*b)) {
        return value.to_owned();
    }
    let end = rest.iter().position(|b| !keep(*b)).unwrap_or(rest.len());
    let mut out = String::with_capacity(value.len());
    push_str_range(&mut out, value, 0, after);
    out.push_str("...");
    push_str_range(&mut out, value, after + end, value.len());
    out
}

fn replace_wal_address(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = String::with_capacity(value.len());
    let mut idx = 0;
    while idx < bytes.len() {
        if !bytes[idx].is_ascii_hexdigit() {
            out.push(char::from(bytes[idx]));
            idx += 1;
            continue;
        }
        let start = idx;
        while idx < bytes.len() && bytes[idx].is_ascii_hexdigit() {
            idx += 1;
        }
        let first_len = idx - start;
        if idx >= bytes.len() || bytes[idx] != b'/' || !(1..=8).contains(&first_len) {
            push_str_range(&mut out, value, start, idx);
            continue;
        }
        idx += 1;
        let second_start = idx;
        while idx < bytes.len() && bytes[idx].is_ascii_hexdigit() {
            idx += 1;
        }
        let second_len = idx - second_start;
        let before_ok = start == 0 || !bytes[start - 1].is_ascii_alphanumeric();
        let after_ok = idx >= bytes.len() || !bytes[idx].is_ascii_alphanumeric();
        if (1..=8).contains(&second_len) && before_ok && after_ok {
            out.push_str("x/x");
        } else {
            push_str_range(&mut out, value, start, idx);
        }
    }
    out
}

fn push_str_range(out: &mut String, value: &str, start: usize, end: usize) {
    if let Some(slice) = value.get(start..end) {
        out.push_str(slice);
    }
}

#[cfg(test)]
mod tests {
    use super::{ErrorCategory, classify_error, normalize_error};
    use crate::parser::LogSeverity;

    #[test]
    fn normalizes_values_for_grouping() {
        assert_eq!(
            normalize_error("relation \"users\" does not exist at character 15"),
            "relation \"...\" does not exist"
        );
        assert_eq!(
            normalize_error("invalid input syntax for type integer: \"abc\""),
            "invalid input syntax for type integer: ..."
        );
        assert_eq!(
            normalize_error("server process (PID 4242) was terminated by signal 9: Killed"),
            "server process (...) was terminated by signal ...: Killed"
        );
    }

    #[test]
    fn classifies_in_contract_order() {
        assert_eq!(
            classify_error(
                "deadlock detected while permission denied",
                LogSeverity::Error
            ),
            ErrorCategory::Lock
        );
        assert_eq!(
            classify_error("could not serialize access", LogSeverity::Error),
            ErrorCategory::Serialization
        );
        assert_eq!(
            classify_error("division by zero", LogSeverity::Error),
            ErrorCategory::Syntax
        );
        assert_eq!(
            classify_error(
                "server process (...) was terminated by signal ...: Killed",
                LogSeverity::Log
            ),
            ErrorCategory::Resource
        );
        assert_eq!(
            classify_error(
                "server process (...) was terminated by signal ...: Segmentation fault",
                LogSeverity::Log
            ),
            ErrorCategory::System
        );
    }

    #[test]
    fn panic_and_fatal_overrides_match_the_floor() {
        assert_eq!(
            classify_error("anything", LogSeverity::Panic),
            ErrorCategory::DataCorruption
        );
        assert_eq!(
            classify_error("uncategorized", LogSeverity::Fatal),
            ErrorCategory::System
        );
    }
}

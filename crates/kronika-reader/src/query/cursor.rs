//! Opaque keyset-pagination cursor.
//!
//! A [`Cursor`] pins the last row a page returned: its `source_id` plus that
//! row's values on the logical section's union columns. Resuming a query drops
//! every row at or before this position under the section's total order, so
//! pages tile the stream with no gap or repeat even across unit boundaries.
//!
//! [`Cursor::encode`] and [`Cursor::decode`] round-trip the cursor through a
//! URL-safe text form. The wire shape is internal: only the round-trip and URL
//! safety are contractual, so the encoding may change without notice.

use crate::query::QueryError;
use crate::query::value::Value;

/// A resume point for keyset pagination over one logical section.
#[derive(Debug, Clone, PartialEq)]
pub struct Cursor {
    /// Source the paged rows belong to; a resume against another source fails.
    pub(crate) source_id: u64,
    /// Last returned row's values, in union-column order.
    pub(crate) values: Vec<Value>,
}

// Wire tags, one per `Value` variant. Stable within an encoded cursor; only the
// round-trip is contractual, so these may be renumbered if the format changes.
const TAG_NULL: u8 = 0;
const TAG_I64: u8 = 1;
const TAG_U64: u8 = 2;
const TAG_F64: u8 = 3;
const TAG_BOOL: u8 = 4;
const TAG_TS: u8 = 5;
const TAG_STR: u8 = 6;
const TAG_BLOB: u8 = 7;
const TAG_LIST_I32: u8 = 8;

impl Cursor {
    /// Encode the cursor as a URL-safe, opaque string.
    #[must_use]
    pub fn encode(&self) -> String {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&self.source_id.to_le_bytes());
        bytes.extend_from_slice(&(self.values.len() as u64).to_le_bytes());
        for value in &self.values {
            encode_value(value, &mut bytes);
        }
        hex_encode(&bytes)
    }

    /// Decode a cursor produced by [`Cursor::encode`].
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::BadCursor`] for any malformed input: non-hex text,
    /// a truncated body, an unknown value tag, or trailing bytes.
    pub fn decode(s: &str) -> Result<Self, QueryError> {
        let bytes = hex_decode(s)?;
        let mut reader = ByteReader::new(&bytes);
        let source_id = reader.take_u64()?;
        // The count is untrusted, so grow the vec as values arrive rather than
        // pre-sizing to a length a malformed cursor could inflate.
        let count = reader.take_u64()?;
        let mut values = Vec::new();
        for _ in 0..count {
            values.push(decode_value(&mut reader)?);
        }
        if !reader.is_empty() {
            return Err(QueryError::BadCursor("trailing bytes after cursor".into()));
        }
        Ok(Self { source_id, values })
    }
}

/// Append one length-prefixed, tagged value to `out`.
fn encode_value(value: &Value, out: &mut Vec<u8>) {
    match value {
        Value::Null => out.push(TAG_NULL),
        Value::I64(v) => {
            out.push(TAG_I64);
            out.extend_from_slice(&v.to_le_bytes());
        }
        Value::U64(v) => {
            out.push(TAG_U64);
            out.extend_from_slice(&v.to_le_bytes());
        }
        Value::F64(v) => {
            out.push(TAG_F64);
            out.extend_from_slice(&v.to_bits().to_le_bytes());
        }
        Value::Bool(v) => {
            out.push(TAG_BOOL);
            out.push(u8::from(*v));
        }
        Value::Ts(v) => {
            out.push(TAG_TS);
            out.extend_from_slice(&v.to_le_bytes());
        }
        Value::Str(s) => {
            out.push(TAG_STR);
            encode_bytes(s.as_bytes(), out);
        }
        Value::Blob {
            text,
            full_len,
            truncated,
        } => {
            out.push(TAG_BLOB);
            encode_bytes(text.as_bytes(), out);
            out.extend_from_slice(&full_len.to_le_bytes());
            out.push(u8::from(*truncated));
        }
        Value::ListI32(list) => {
            out.push(TAG_LIST_I32);
            out.extend_from_slice(&(list.len() as u64).to_le_bytes());
            for item in list {
                out.extend_from_slice(&item.to_le_bytes());
            }
        }
    }
}

/// Length-prefix (u64 LE) then append raw bytes.
fn encode_bytes(bytes: &[u8], out: &mut Vec<u8>) {
    out.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
    out.extend_from_slice(bytes);
}

/// Read one tagged value at the reader's cursor.
fn decode_value(reader: &mut ByteReader<'_>) -> Result<Value, QueryError> {
    let tag = reader.take_u8()?;
    match tag {
        TAG_NULL => Ok(Value::Null),
        TAG_I64 => Ok(Value::I64(i64::from_le_bytes(reader.take_array()?))),
        TAG_U64 => Ok(Value::U64(u64::from_le_bytes(reader.take_array()?))),
        TAG_F64 => Ok(Value::F64(f64::from_bits(u64::from_le_bytes(
            reader.take_array()?,
        )))),
        TAG_BOOL => Ok(Value::Bool(reader.take_bool()?)),
        TAG_TS => Ok(Value::Ts(i64::from_le_bytes(reader.take_array()?))),
        TAG_STR => Ok(Value::Str(reader.take_utf8()?)),
        TAG_BLOB => {
            let text = reader.take_utf8()?;
            let full_len = reader.take_u64()?;
            let truncated = reader.take_bool()?;
            Ok(Value::Blob {
                text,
                full_len,
                truncated,
            })
        }
        TAG_LIST_I32 => {
            // Untrusted count: grow as items arrive instead of pre-sizing.
            let count = reader.take_u64()?;
            let mut list = Vec::new();
            for _ in 0..count {
                list.push(i32::from_le_bytes(reader.take_array()?));
            }
            Ok(Value::ListI32(list))
        }
        other => Err(QueryError::BadCursor(format!("unknown value tag {other}"))),
    }
}

/// A forward-only reader over cursor bytes; every short read is a `BadCursor`.
struct ByteReader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> ByteReader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    const fn is_empty(&self) -> bool {
        self.pos >= self.bytes.len()
    }

    fn take_slice(&mut self, len: usize) -> Result<&'a [u8], QueryError> {
        let end = self
            .pos
            .checked_add(len)
            .filter(|&end| end <= self.bytes.len())
            .ok_or_else(|| QueryError::BadCursor("cursor ended mid-value".into()))?;
        let slice = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    /// Read exactly `N` bytes into a fixed array. `take_slice(N)` yields a slice
    /// of length `N`, so `copy_from_slice` cannot panic here.
    fn take_array<const N: usize>(&mut self) -> Result<[u8; N], QueryError> {
        let slice = self.take_slice(N)?;
        let mut array = [0_u8; N];
        array.copy_from_slice(slice);
        Ok(array)
    }

    fn take_u8(&mut self) -> Result<u8, QueryError> {
        Ok(self.take_array::<1>()?[0])
    }

    fn take_u64(&mut self) -> Result<u64, QueryError> {
        Ok(u64::from_le_bytes(self.take_array()?))
    }

    fn take_bool(&mut self) -> Result<bool, QueryError> {
        match self.take_u8()? {
            0 => Ok(false),
            1 => Ok(true),
            other => Err(QueryError::BadCursor(format!("invalid bool byte {other}"))),
        }
    }

    fn take_utf8(&mut self) -> Result<String, QueryError> {
        let len = self.take_u64()?;
        let len = usize::try_from(len)
            .map_err(|_overflow| QueryError::BadCursor("cursor length exceeds usize".into()))?;
        let slice = self.take_slice(len)?;
        Ok(String::from_utf8_lossy(slice).into_owned())
    }
}

/// Encode bytes as lowercase hex.
fn hex_encode(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(DIGITS[usize::from(byte >> 4)] as char);
        out.push(DIGITS[usize::from(byte & 0x0f)] as char);
    }
    out
}

/// Decode lowercase-or-uppercase hex; any non-hex or odd length is a `BadCursor`.
fn hex_decode(s: &str) -> Result<Vec<u8>, QueryError> {
    let bytes = s.as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return Err(QueryError::BadCursor("odd-length hex cursor".into()));
    }
    let mut out = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.chunks_exact(2) {
        let hi = hex_nibble(pair[0])?;
        let lo = hex_nibble(pair[1])?;
        out.push((hi << 4) | lo);
    }
    Ok(out)
}

/// One hex digit to its 0-15 value.
fn hex_nibble(byte: u8) -> Result<u8, QueryError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        other => Err(QueryError::BadCursor(format!(
            "non-hex byte {other:#04x} in cursor"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::{Cursor, hex_encode};
    use crate::query::QueryError;
    use crate::query::value::Value;

    /// One value of every variant, to exercise each encode/decode arm.
    fn all_variants() -> Vec<Value> {
        vec![
            Value::Null,
            Value::I64(-9_223_372_036_854_775_808),
            Value::U64(18_446_744_073_709_551_615),
            Value::F64(-0.0),
            Value::Bool(true),
            Value::Bool(false),
            Value::Ts(1_700_000_000_000_000),
            Value::Str("héllo, мир".to_owned()),
            Value::Blob {
                text: "SELECT".to_owned(),
                full_len: 4096,
                truncated: true,
            },
            Value::ListI32(vec![i32::MIN, 0, i32::MAX]),
        ]
    }

    #[test]
    fn round_trip_preserves_every_value_variant() {
        let cursor = Cursor {
            source_id: 42,
            values: all_variants(),
        };
        let decoded = Cursor::decode(&cursor.encode()).expect("decode");
        assert_eq!(decoded, cursor);
    }

    #[test]
    fn round_trip_preserves_empty_values() {
        let cursor = Cursor {
            source_id: 0,
            values: Vec::new(),
        };
        let decoded = Cursor::decode(&cursor.encode()).expect("decode");
        assert_eq!(decoded, cursor);
    }

    #[test]
    fn encode_is_stable_for_the_same_position() {
        let cursor = Cursor {
            source_id: 7,
            values: all_variants(),
        };
        assert_eq!(cursor.encode(), cursor.encode());
    }

    #[test]
    fn encode_is_url_safe() {
        let cursor = Cursor {
            source_id: u64::MAX,
            values: all_variants(),
        };
        let encoded = cursor.encode();
        assert!(
            encoded.bytes().all(|b| b.is_ascii_hexdigit()),
            "hex output is URL-safe: {encoded}"
        );
    }

    #[test]
    fn decode_rejects_non_hex_text() {
        let err = Cursor::decode("not-a-cursor").unwrap_err();
        assert!(matches!(err, QueryError::BadCursor(_)), "got {err:?}");
    }

    #[test]
    fn decode_rejects_odd_length_hex() {
        let err = Cursor::decode("abc").unwrap_err();
        assert!(matches!(err, QueryError::BadCursor(_)), "got {err:?}");
    }

    #[test]
    fn decode_rejects_truncated_body() {
        // Two hex digits: one byte, far short of the source_id header.
        let err = Cursor::decode("ff").unwrap_err();
        assert!(matches!(err, QueryError::BadCursor(_)), "got {err:?}");
    }

    #[test]
    fn decode_rejects_unknown_tag() {
        // source_id (8 bytes) + count 1 (8 bytes) + tag 0xFF (no such variant).
        let mut bytes = vec![0_u8; 8];
        bytes.extend_from_slice(&1_u64.to_le_bytes());
        bytes.push(0xff);
        let err = Cursor::decode(&hex_encode(&bytes)).unwrap_err();
        assert!(matches!(err, QueryError::BadCursor(_)), "got {err:?}");
    }

    #[test]
    fn decode_rejects_trailing_bytes() {
        let cursor = Cursor {
            source_id: 1,
            values: vec![Value::I64(1)],
        };
        let mut encoded = cursor.encode();
        encoded.push_str("00");
        let err = Cursor::decode(&encoded).unwrap_err();
        assert!(matches!(err, QueryError::BadCursor(_)), "got {err:?}");
    }
}

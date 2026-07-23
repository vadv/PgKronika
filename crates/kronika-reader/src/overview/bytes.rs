//! Bounded little-endian cursors for the fact-file codec.
//!
//! The reader never trusts a length it has not checked. Every variable-length
//! read takes an explicit hard bound and refuses to allocate or slice past it,
//! so a corrupt or hostile length yields a typed error instead of a panic or a
//! giant allocation. Integers are fixed-width little-endian; counts and byte
//! lengths use a canonical `LEB128` varint that rejects overlong encodings.

/// Why raw block bytes failed structural decoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ByteError {
    /// A read needed more bytes than the input held.
    Truncated,
    /// A varint ran past the ten-byte ceiling for a `u64`.
    VarintTooLong,
    /// A varint used a longer encoding than the canonical minimum.
    VarintNonCanonical,
    /// A decoded length or count exceeded the caller's hard bound.
    AboveBound,
    /// The decoder stopped with bytes still unread in the block.
    TrailingBytes,
    /// A float field decoded to `NaN` or an infinity.
    NonFiniteFloat,
}

/// A forward-only reader over a byte block with bounds checks on every read.
#[derive(Debug)]
pub(crate) struct ByteReader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> ByteReader<'a> {
    /// Wraps a block for reading from its start.
    pub(crate) const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    /// Bytes not yet consumed.
    pub(crate) const fn remaining(&self) -> usize {
        self.bytes.len() - self.pos
    }

    /// Confirms the whole block was consumed, per the canonical decode rule.
    ///
    /// # Errors
    /// Returns [`ByteError::TrailingBytes`] when unread bytes remain.
    pub(crate) const fn finish(&self) -> Result<(), ByteError> {
        if self.remaining() == 0 {
            Ok(())
        } else {
            Err(ByteError::TrailingBytes)
        }
    }

    /// Borrows the next `len` bytes.
    ///
    /// # Errors
    /// Returns [`ByteError::Truncated`] when fewer than `len` bytes remain.
    pub(crate) fn take(&mut self, len: usize) -> Result<&'a [u8], ByteError> {
        let end = self.pos.checked_add(len).ok_or(ByteError::Truncated)?;
        let slice = self.bytes.get(self.pos..end).ok_or(ByteError::Truncated)?;
        self.pos = end;
        Ok(slice)
    }

    /// Reads a fixed-width byte array.
    ///
    /// # Errors
    /// Returns [`ByteError::Truncated`] when fewer than `N` bytes remain.
    pub(crate) fn array<const N: usize>(&mut self) -> Result<[u8; N], ByteError> {
        let mut out = [0_u8; N];
        out.copy_from_slice(self.take(N)?);
        Ok(out)
    }

    /// Reads one byte.
    ///
    /// # Errors
    /// Returns [`ByteError::Truncated`] at end of input.
    pub(crate) fn u8(&mut self) -> Result<u8, ByteError> {
        Ok(self.array::<1>()?[0])
    }

    /// Reads a little-endian `u16`.
    ///
    /// # Errors
    /// Returns [`ByteError::Truncated`] when fewer than two bytes remain.
    pub(crate) fn u16_le(&mut self) -> Result<u16, ByteError> {
        Ok(u16::from_le_bytes(self.array()?))
    }

    /// Reads a little-endian `u32`.
    ///
    /// # Errors
    /// Returns [`ByteError::Truncated`] when fewer than four bytes remain.
    pub(crate) fn u32_le(&mut self) -> Result<u32, ByteError> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    /// Reads a little-endian `u64`.
    ///
    /// # Errors
    /// Returns [`ByteError::Truncated`] when fewer than eight bytes remain.
    pub(crate) fn u64_le(&mut self) -> Result<u64, ByteError> {
        Ok(u64::from_le_bytes(self.array()?))
    }

    /// Reads a little-endian `i32`.
    ///
    /// # Errors
    /// Returns [`ByteError::Truncated`] when fewer than four bytes remain.
    pub(crate) fn i32_le(&mut self) -> Result<i32, ByteError> {
        Ok(i32::from_le_bytes(self.array()?))
    }

    /// Reads a little-endian `i64`.
    ///
    /// # Errors
    /// Returns [`ByteError::Truncated`] when fewer than eight bytes remain.
    pub(crate) fn i64_le(&mut self) -> Result<i64, ByteError> {
        Ok(i64::from_le_bytes(self.array()?))
    }

    /// Reads a finite little-endian `binary64`.
    ///
    /// # Errors
    /// Returns [`ByteError::NonFiniteFloat`] for `NaN` or an infinity, and
    /// [`ByteError::Truncated`] when fewer than eight bytes remain.
    pub(crate) fn f64_finite(&mut self) -> Result<f64, ByteError> {
        let value = f64::from_le_bytes(self.array()?);
        if value.is_finite() {
            Ok(value)
        } else {
            Err(ByteError::NonFiniteFloat)
        }
    }

    /// Reads a canonical `LEB128` unsigned varint, rejecting `> bound`.
    ///
    /// # Errors
    /// Returns [`ByteError::VarintTooLong`] past ten bytes,
    /// [`ByteError::VarintNonCanonical`] for an overlong encoding, and
    /// [`ByteError::AboveBound`] when the value exceeds `bound`.
    pub(crate) fn uvarint(&mut self, bound: u64) -> Result<u64, ByteError> {
        let mut value = 0_u64;
        let mut shift = 0_u32;
        loop {
            let byte = self.u8()?;
            let group = u64::from(byte & 0x7F);
            // The tenth group only leaves one usable bit before `u64` overflows.
            if shift == 63 && group > 1 {
                return Err(ByteError::VarintTooLong);
            }
            value |= group << shift;
            if byte & 0x80 == 0 {
                // Canonical form: a multi-byte encoding never ends in a zero
                // group, which would be a shorter value spelled the long way.
                if shift > 0 && byte == 0 {
                    return Err(ByteError::VarintNonCanonical);
                }
                break;
            }
            shift += 7;
            if shift >= 70 {
                return Err(ByteError::VarintTooLong);
            }
        }
        if value > bound {
            return Err(ByteError::AboveBound);
        }
        Ok(value)
    }

    /// Reads a varint length `<= bound`, then borrows that many bytes.
    ///
    /// # Errors
    /// Returns [`ByteError`] for an out-of-bound length or a truncated body.
    pub(crate) fn length_prefixed(&mut self, bound: u64) -> Result<&'a [u8], ByteError> {
        let len = self.uvarint(bound)?;
        let len = usize::try_from(len).ok().ok_or(ByteError::AboveBound)?;
        self.take(len)
    }
}

/// An append-only little-endian block builder.
#[derive(Debug, Default)]
pub(crate) struct ByteWriter {
    buf: Vec<u8>,
}

impl ByteWriter {
    /// A new empty writer.
    pub(crate) const fn new() -> Self {
        Self { buf: Vec::new() }
    }

    /// Consumes the writer and returns its bytes.
    pub(crate) fn into_bytes(self) -> Vec<u8> {
        self.buf
    }

    /// Appends raw bytes verbatim.
    pub(crate) fn bytes(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Appends one byte.
    pub(crate) fn u8(&mut self, value: u8) {
        self.buf.push(value);
    }

    /// Appends a little-endian `u16`.
    pub(crate) fn u16_le(&mut self, value: u16) {
        self.buf.extend_from_slice(&value.to_le_bytes());
    }

    /// Appends a little-endian `u32`.
    pub(crate) fn u32_le(&mut self, value: u32) {
        self.buf.extend_from_slice(&value.to_le_bytes());
    }

    /// Appends a little-endian `u64`.
    pub(crate) fn u64_le(&mut self, value: u64) {
        self.buf.extend_from_slice(&value.to_le_bytes());
    }

    /// Appends a little-endian `i32`.
    pub(crate) fn i32_le(&mut self, value: i32) {
        self.buf.extend_from_slice(&value.to_le_bytes());
    }

    /// Appends a little-endian `i64`.
    pub(crate) fn i64_le(&mut self, value: i64) {
        self.buf.extend_from_slice(&value.to_le_bytes());
    }

    /// Appends a little-endian `binary64`.
    pub(crate) fn f64_le(&mut self, value: f64) {
        self.buf.extend_from_slice(&value.to_le_bytes());
    }

    /// Appends a canonical `LEB128` unsigned varint.
    pub(crate) fn uvarint(&mut self, mut value: u64) {
        loop {
            let mut byte = (value & 0x7F) as u8;
            value >>= 7;
            if value != 0 {
                byte |= 0x80;
            }
            self.buf.push(byte);
            if value == 0 {
                break;
            }
        }
    }

    /// Appends a varint length prefix followed by the bytes.
    pub(crate) fn length_prefixed(&mut self, bytes: &[u8]) {
        self.uvarint(bytes.len() as u64);
        self.bytes(bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::{ByteError, ByteReader, ByteWriter};

    #[test]
    fn fixed_width_integers_round_trip_little_endian() {
        let mut writer = ByteWriter::new();
        writer.u16_le(0x1234);
        writer.u32_le(0x89AB_CDEF);
        writer.u64_le(0x0102_0304_0506_0708);
        writer.i64_le(-9);
        writer.f64_le(2.5);
        let bytes = writer.into_bytes();

        let mut reader = ByteReader::new(&bytes);
        assert_eq!(reader.u16_le(), Ok(0x1234));
        assert_eq!(reader.u32_le(), Ok(0x89AB_CDEF));
        assert_eq!(reader.u64_le(), Ok(0x0102_0304_0506_0708));
        assert_eq!(reader.i64_le(), Ok(-9));
        assert_eq!(reader.f64_finite(), Ok(2.5));
        assert_eq!(reader.finish(), Ok(()));
    }

    #[test]
    fn a_short_read_is_truncated_not_a_panic() {
        let bytes = [0_u8; 3];
        let mut reader = ByteReader::new(&bytes);
        assert_eq!(reader.u32_le(), Err(ByteError::Truncated));
    }

    #[test]
    fn trailing_bytes_invalidate_a_block() {
        let bytes = [1_u8, 2, 3];
        let mut reader = ByteReader::new(&bytes);
        assert_eq!(reader.u8(), Ok(1));
        assert_eq!(reader.finish(), Err(ByteError::TrailingBytes));
    }

    #[test]
    fn a_non_finite_float_is_rejected() {
        let mut writer = ByteWriter::new();
        writer.f64_le(f64::NAN);
        let bytes = writer.into_bytes();
        let mut reader = ByteReader::new(&bytes);
        assert_eq!(reader.f64_finite(), Err(ByteError::NonFiniteFloat));
    }

    #[test]
    fn varints_round_trip_across_the_whole_u64_range() {
        for value in [
            0_u64,
            1,
            127,
            128,
            300,
            u64::from(u32::MAX),
            u64::MAX - 1,
            u64::MAX,
        ] {
            let mut writer = ByteWriter::new();
            writer.uvarint(value);
            let bytes = writer.into_bytes();
            let mut reader = ByteReader::new(&bytes);
            assert_eq!(reader.uvarint(u64::MAX), Ok(value), "value {value}");
            assert_eq!(reader.finish(), Ok(()), "value {value}");
        }
    }

    #[test]
    fn a_varint_above_its_bound_is_rejected_before_use() {
        let mut writer = ByteWriter::new();
        writer.uvarint(5_000);
        let bytes = writer.into_bytes();
        let mut reader = ByteReader::new(&bytes);
        assert_eq!(reader.uvarint(4_096), Err(ByteError::AboveBound));
    }

    #[test]
    fn an_overlong_varint_encoding_is_rejected() {
        // 0x80 0x00 spells zero the long way; the canonical form is 0x00.
        let bytes = [0x80_u8, 0x00];
        let mut reader = ByteReader::new(&bytes);
        assert_eq!(reader.uvarint(u64::MAX), Err(ByteError::VarintNonCanonical));
    }

    #[test]
    fn a_varint_past_ten_bytes_or_overflowing_u64_is_rejected() {
        let eleven_continuations = [0x80_u8; 11];
        let mut reader = ByteReader::new(&eleven_continuations);
        assert_eq!(reader.uvarint(u64::MAX), Err(ByteError::VarintTooLong));

        // Ten bytes whose tenth group sets more than the one legal bit.
        let overflow = [
            0x80_u8, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x02,
        ];
        let mut reader = ByteReader::new(&overflow);
        assert_eq!(reader.uvarint(u64::MAX), Err(ByteError::VarintTooLong));
    }

    #[test]
    fn a_length_prefixed_read_refuses_an_unbacked_length() {
        let mut writer = ByteWriter::new();
        writer.uvarint(1_000);
        let bytes = writer.into_bytes();
        let mut reader = ByteReader::new(&bytes);
        // The length is within bound but the body is not present.
        assert_eq!(reader.length_prefixed(4_096), Err(ByteError::Truncated));
    }

    #[test]
    fn length_prefixed_bytes_round_trip() {
        let mut writer = ByteWriter::new();
        writer.length_prefixed(b"pattern");
        let bytes = writer.into_bytes();
        let mut reader = ByteReader::new(&bytes);
        assert_eq!(reader.length_prefixed(4_096), Ok(b"pattern".as_slice()));
        assert_eq!(reader.finish(), Ok(()));
    }
}

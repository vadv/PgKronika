use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

use super::MAX_FRAME_CURSOR_BYTES;

const CURSOR_VERSION: u8 = 1;
const MAX_TEXT_PREFIX_BYTES: usize = 64;
const MAX_CURSOR_PAYLOAD_BYTES: usize = MAX_FRAME_CURSOR_BYTES * 3 / 4;

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum SortKey {
    Null,
    Signed(i64),
    Unsigned(u64),
    Float(f64),
    Boolean(bool),
    Timestamp(i64),
    TextPrefix(Vec<u8>),
}

impl SortKey {
    pub(crate) fn text_prefix(value: &str) -> Self {
        let mut end = value.len().min(MAX_TEXT_PREFIX_BYTES);
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        Self::TextPrefix(value.as_bytes()[..end].to_vec())
    }

    fn validate(&self) -> Result<(), CursorError> {
        match self {
            Self::Float(value) if !value.is_finite() => Err(CursorError::Invalid),
            Self::TextPrefix(value)
                if value.len() > MAX_TEXT_PREFIX_BYTES || std::str::from_utf8(value).is_err() =>
            {
                Err(CursorError::Invalid)
            }
            Self::Null
            | Self::Signed(_)
            | Self::Unsigned(_)
            | Self::Float(_)
            | Self::Boolean(_)
            | Self::Timestamp(_)
            | Self::TextPrefix(_) => Ok(()),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct FrameCursor {
    version: u8,
    view_code: u16,
    view_revision: u16,
    snapshot_ts_us: i64,
    query_fingerprint: [u8; 32],
    sort_key: SortKey,
    entity: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CursorError {
    Invalid,
    AboveBound,
}

impl FrameCursor {
    pub(crate) fn new(
        view_code: u16,
        view_revision: u16,
        snapshot_ts_us: i64,
        query_fingerprint: [u8; 32],
        sort_key: SortKey,
        entity: Vec<u8>,
    ) -> Result<Self, CursorError> {
        let max_entity_bytes = usize::try_from(kronika_reader::LIMIT.web_identity_bytes)
            .map_err(|_error| CursorError::AboveBound)?;
        if view_code == 0 || view_revision == 0 || entity.len() > max_entity_bytes {
            return Err(CursorError::AboveBound);
        }
        sort_key.validate()?;
        let cursor = Self {
            version: CURSOR_VERSION,
            view_code,
            view_revision,
            snapshot_ts_us,
            query_fingerprint,
            sort_key,
            entity,
        };
        cursor.encode()?;
        Ok(cursor)
    }

    pub(crate) fn encode(&self) -> Result<String, CursorError> {
        let max_entity_bytes = usize::try_from(kronika_reader::LIMIT.web_identity_bytes)
            .map_err(|_error| CursorError::AboveBound)?;
        if self.version != CURSOR_VERSION
            || self.view_code == 0
            || self.view_revision == 0
            || self.entity.len() > max_entity_bytes
        {
            return Err(CursorError::Invalid);
        }
        self.sort_key.validate()?;

        let mut payload = Vec::with_capacity(MAX_CURSOR_PAYLOAD_BYTES);
        payload.push(self.version);
        payload.extend_from_slice(&self.view_code.to_le_bytes());
        payload.extend_from_slice(&self.view_revision.to_le_bytes());
        payload.extend_from_slice(&self.snapshot_ts_us.to_le_bytes());
        payload.extend_from_slice(&self.query_fingerprint);
        encode_sort_key(&mut payload, &self.sort_key)?;
        let entity_len =
            u16::try_from(self.entity.len()).map_err(|_error| CursorError::AboveBound)?;
        payload.extend_from_slice(&entity_len.to_le_bytes());
        payload.extend_from_slice(&self.entity);
        if payload.len() > MAX_CURSOR_PAYLOAD_BYTES {
            return Err(CursorError::AboveBound);
        }

        let encoded = URL_SAFE_NO_PAD.encode(payload);
        if encoded.len() > MAX_FRAME_CURSOR_BYTES {
            return Err(CursorError::AboveBound);
        }
        Ok(encoded)
    }

    pub(crate) fn decode(encoded: &str) -> Result<Self, CursorError> {
        if encoded.is_empty() || encoded.len() > MAX_FRAME_CURSOR_BYTES {
            return Err(CursorError::AboveBound);
        }
        let payload = URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(|_error| CursorError::Invalid)?;
        if payload.len() > MAX_CURSOR_PAYLOAD_BYTES {
            return Err(CursorError::AboveBound);
        }
        let mut reader = PayloadReader::new(&payload);
        let version = reader.u8()?;
        if version != CURSOR_VERSION {
            return Err(CursorError::Invalid);
        }
        let view_code = reader.u16()?;
        let view_revision = reader.u16()?;
        let snapshot_ts_us = reader.i64()?;
        let query_fingerprint = reader.array_32()?;
        let sort_key = decode_sort_key(&mut reader)?;
        let entity_len = usize::from(reader.u16()?);
        let max_entity_bytes = usize::try_from(kronika_reader::LIMIT.web_identity_bytes)
            .map_err(|_error| CursorError::AboveBound)?;
        if entity_len > max_entity_bytes {
            return Err(CursorError::AboveBound);
        }
        let entity = reader.take(entity_len)?.to_vec();
        reader.finish()?;
        Self::new(
            view_code,
            view_revision,
            snapshot_ts_us,
            query_fingerprint,
            sort_key,
            entity,
        )
    }

    pub(crate) const fn view_code(&self) -> u16 {
        self.view_code
    }

    pub(crate) const fn view_revision(&self) -> u16 {
        self.view_revision
    }

    pub(crate) const fn snapshot_ts_us(&self) -> i64 {
        self.snapshot_ts_us
    }

    pub(crate) const fn query_fingerprint(&self) -> [u8; 32] {
        self.query_fingerprint
    }

    pub(crate) const fn sort_key(&self) -> &SortKey {
        &self.sort_key
    }

    pub(crate) fn entity(&self) -> &[u8] {
        &self.entity
    }
}

fn encode_sort_key(payload: &mut Vec<u8>, key: &SortKey) -> Result<(), CursorError> {
    match key {
        SortKey::Null => payload.push(0),
        SortKey::Signed(value) => {
            payload.push(1);
            payload.extend_from_slice(&value.to_le_bytes());
        }
        SortKey::Unsigned(value) => {
            payload.push(2);
            payload.extend_from_slice(&value.to_le_bytes());
        }
        SortKey::Float(value) => {
            if !value.is_finite() {
                return Err(CursorError::Invalid);
            }
            payload.push(3);
            payload.extend_from_slice(&value.to_bits().to_le_bytes());
        }
        SortKey::Boolean(value) => {
            payload.push(4);
            payload.push(u8::from(*value));
        }
        SortKey::Timestamp(value) => {
            payload.push(5);
            payload.extend_from_slice(&value.to_le_bytes());
        }
        SortKey::TextPrefix(value) => {
            let len = u8::try_from(value.len()).map_err(|_error| CursorError::AboveBound)?;
            payload.push(6);
            payload.push(len);
            payload.extend_from_slice(value);
        }
    }
    Ok(())
}

fn decode_sort_key(reader: &mut PayloadReader<'_>) -> Result<SortKey, CursorError> {
    match reader.u8()? {
        0 => Ok(SortKey::Null),
        1 => Ok(SortKey::Signed(reader.i64()?)),
        2 => Ok(SortKey::Unsigned(reader.u64()?)),
        3 => {
            let value = f64::from_bits(reader.u64()?);
            value
                .is_finite()
                .then_some(SortKey::Float(value))
                .ok_or(CursorError::Invalid)
        }
        4 => match reader.u8()? {
            0 => Ok(SortKey::Boolean(false)),
            1 => Ok(SortKey::Boolean(true)),
            _ => Err(CursorError::Invalid),
        },
        5 => Ok(SortKey::Timestamp(reader.i64()?)),
        6 => {
            let len = usize::from(reader.u8()?);
            if len > MAX_TEXT_PREFIX_BYTES {
                return Err(CursorError::AboveBound);
            }
            let value = reader.take(len)?.to_vec();
            std::str::from_utf8(&value).map_err(|_error| CursorError::Invalid)?;
            Ok(SortKey::TextPrefix(value))
        }
        _ => Err(CursorError::Invalid),
    }
}

struct PayloadReader<'a> {
    payload: &'a [u8],
    at: usize,
}

impl<'a> PayloadReader<'a> {
    const fn new(payload: &'a [u8]) -> Self {
        Self { payload, at: 0 }
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], CursorError> {
        let end = self.at.checked_add(len).ok_or(CursorError::Invalid)?;
        let bytes = self.payload.get(self.at..end).ok_or(CursorError::Invalid)?;
        self.at = end;
        Ok(bytes)
    }

    fn u8(&mut self) -> Result<u8, CursorError> {
        self.take(1)?.first().copied().ok_or(CursorError::Invalid)
    }

    fn u16(&mut self) -> Result<u16, CursorError> {
        Ok(u16::from_le_bytes(
            self.take(2)?
                .try_into()
                .map_err(|_error| CursorError::Invalid)?,
        ))
    }

    fn u64(&mut self) -> Result<u64, CursorError> {
        Ok(u64::from_le_bytes(
            self.take(8)?
                .try_into()
                .map_err(|_error| CursorError::Invalid)?,
        ))
    }

    fn i64(&mut self) -> Result<i64, CursorError> {
        Ok(i64::from_le_bytes(
            self.take(8)?
                .try_into()
                .map_err(|_error| CursorError::Invalid)?,
        ))
    }

    fn array_32(&mut self) -> Result<[u8; 32], CursorError> {
        self.take(32)?
            .try_into()
            .map_err(|_error| CursorError::Invalid)
    }

    fn finish(self) -> Result<(), CursorError> {
        (self.at == self.payload.len())
            .then_some(())
            .ok_or(CursorError::Invalid)
    }
}

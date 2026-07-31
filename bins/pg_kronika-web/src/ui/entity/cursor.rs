use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

const CURSOR_REVISION: u8 = 1;
const MAX_CURSOR_BYTES: usize = 512;
const PAYLOAD_BYTES: usize = 1 + 2 + 2 + 8 + 8 + 8 + 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EntityHistoryCursor {
    view_code: u16,
    view_revision: u16,
    from_us: i64,
    to_us: i64,
    last_ts_us: i64,
    fingerprint: [u8; 32],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CursorError {
    Invalid,
    AboveBound,
}

impl EntityHistoryCursor {
    pub(crate) const fn new(
        view_code: u16,
        view_revision: u16,
        from_us: i64,
        to_us: i64,
        last_ts_us: i64,
        fingerprint: [u8; 32],
    ) -> Result<Self, CursorError> {
        if view_code == 0
            || view_revision == 0
            || from_us >= to_us
            || last_ts_us < from_us
            || last_ts_us >= to_us
        {
            return Err(CursorError::Invalid);
        }
        Ok(Self {
            view_code,
            view_revision,
            from_us,
            to_us,
            last_ts_us,
            fingerprint,
        })
    }

    pub(crate) fn encode(self) -> Result<String, CursorError> {
        let mut payload = Vec::with_capacity(PAYLOAD_BYTES);
        payload.push(CURSOR_REVISION);
        payload.extend_from_slice(&self.view_code.to_le_bytes());
        payload.extend_from_slice(&self.view_revision.to_le_bytes());
        payload.extend_from_slice(&self.from_us.to_le_bytes());
        payload.extend_from_slice(&self.to_us.to_le_bytes());
        payload.extend_from_slice(&self.last_ts_us.to_le_bytes());
        payload.extend_from_slice(&self.fingerprint);
        let encoded = URL_SAFE_NO_PAD.encode(payload);
        if encoded.len() > MAX_CURSOR_BYTES {
            return Err(CursorError::AboveBound);
        }
        Ok(encoded)
    }

    pub(crate) fn decode(encoded: &str) -> Result<Self, CursorError> {
        if encoded.is_empty() || encoded.len() > MAX_CURSOR_BYTES {
            return Err(CursorError::AboveBound);
        }
        let payload = URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(|_error| CursorError::Invalid)?;
        if payload.len() != PAYLOAD_BYTES || payload.first() != Some(&CURSOR_REVISION) {
            return Err(CursorError::Invalid);
        }
        let mut at = 1;
        let view_code = read_u16(&payload, &mut at)?;
        let view_revision = read_u16(&payload, &mut at)?;
        let from_us = read_i64(&payload, &mut at)?;
        let to_us = read_i64(&payload, &mut at)?;
        let last_ts_us = read_i64(&payload, &mut at)?;
        let fingerprint = payload
            .get(at..at + 32)
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or(CursorError::Invalid)?;
        Self::new(
            view_code,
            view_revision,
            from_us,
            to_us,
            last_ts_us,
            fingerprint,
        )
    }

    pub(crate) const fn view_code(self) -> u16 {
        self.view_code
    }

    pub(crate) const fn view_revision(self) -> u16 {
        self.view_revision
    }

    pub(crate) const fn range_start_us(self) -> i64 {
        self.from_us
    }

    pub(crate) const fn range_end_us(self) -> i64 {
        self.to_us
    }

    pub(crate) const fn last_ts_us(self) -> i64 {
        self.last_ts_us
    }

    pub(crate) const fn fingerprint(self) -> [u8; 32] {
        self.fingerprint
    }
}

fn read_u16(payload: &[u8], at: &mut usize) -> Result<u16, CursorError> {
    let end = at.checked_add(2).ok_or(CursorError::Invalid)?;
    let value = payload
        .get(*at..end)
        .and_then(|bytes| bytes.try_into().ok())
        .map(u16::from_le_bytes)
        .ok_or(CursorError::Invalid)?;
    *at = end;
    Ok(value)
}

fn read_i64(payload: &[u8], at: &mut usize) -> Result<i64, CursorError> {
    let end = at.checked_add(8).ok_or(CursorError::Invalid)?;
    let value = payload
        .get(*at..end)
        .and_then(|bytes| bytes.try_into().ok())
        .map(i64::from_le_bytes)
        .ok_or(CursorError::Invalid)?;
    *at = end;
    Ok(value)
}

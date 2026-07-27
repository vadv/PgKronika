//! Allocation-safe Parquet footer and page admission before Arrow decode.

use parquet::format::{FileMetaData, PageHeader, PageType};
use parquet::thrift::TSerializable;
use thrift::protocol::{
    TFieldIdentifier, TInputProtocol, TListIdentifier, TMapIdentifier, TMessageIdentifier,
    TSetIdentifier, TStructIdentifier, TType,
};

use crate::codec::MAX_LIST_I32_VALUES_PER_SECTION;
use crate::{
    CodecError, MAX_DECODED_SECTION_BYTES, MAX_ROW_GROUPS, MAX_SECTION_BYTES, MAX_SECTION_ROWS,
};

const MAGIC: &[u8; 4] = b"PAR1";
const MAX_THRIFT_NESTING: usize = 16;
const MAX_THRIFT_ITEMS: usize = MAX_SECTION_ROWS;

/// Validate all allocation-driving Parquet metadata before Arrow sees it.
///
/// The footer and every page header are parsed directly from the bounded body.
/// Footer totals, page claims, value counts, and exact column ranges must agree.
///
/// # Errors
///
/// Returns a typed [`CodecError`] for an oversized body or decoded-work claim,
/// excessive rows or row groups, or an inconsistent Parquet layout.
pub fn validate_parquet_decode_work(
    body: &[u8],
    max_decoded_bytes: usize,
) -> Result<(), CodecError> {
    validate_parquet_decode_profile(body, max_decoded_bytes, true)
}

/// Validate Parquet decode work and reject dictionary pages before Arrow.
///
/// Variable-width dictionary sections use this profile because encoded page
/// sizes do not bound the bytes materialized when dictionary indices expand.
///
/// # Errors
///
/// Returns [`CodecError::DictionaryEncodingUnsupported`] when the footer
/// declares a dictionary page, or the same bounded-layout errors as
/// [`validate_parquet_decode_work`].
pub fn validate_plain_parquet_decode_work(
    body: &[u8],
    max_decoded_bytes: usize,
) -> Result<(), CodecError> {
    validate_parquet_decode_profile(body, max_decoded_bytes, false)
}

fn validate_parquet_decode_profile(
    body: &[u8],
    max_decoded_bytes: usize,
    allow_dictionary: bool,
) -> Result<(), CodecError> {
    if body.len() > MAX_SECTION_BYTES {
        return Err(CodecError::SectionTooLarge {
            len: body.len(),
            max: MAX_SECTION_BYTES,
        });
    }
    if max_decoded_bytes > MAX_DECODED_SECTION_BYTES {
        return Err(CodecError::DecodedSectionTooLarge {
            len: max_decoded_bytes,
            max: MAX_DECODED_SECTION_BYTES,
        });
    }
    let (metadata, metadata_start) = parse_footer(body)?;
    validate_file_metadata(
        body,
        &metadata,
        metadata_start,
        max_decoded_bytes,
        allow_dictionary,
    )
}

fn parse_footer(body: &[u8]) -> Result<(FileMetaData, usize), CodecError> {
    if body.len() < 12 || body.get(..4) != Some(MAGIC) || body.get(body.len() - 4..) != Some(MAGIC)
    {
        return Err(CodecError::InvalidPageLayout);
    }
    let footer_len = u32::from_le_bytes(
        body[body.len() - 8..body.len() - 4]
            .try_into()
            .map_err(|_error| CodecError::InvalidPageLayout)?,
    ) as usize;
    let metadata_end = body.len() - 8;
    let metadata_start = metadata_end
        .checked_sub(footer_len)
        .filter(|&start| start >= MAGIC.len())
        .ok_or(CodecError::InvalidPageLayout)?;
    let mut protocol = BoundedCompactInput::new(&body[metadata_start..metadata_end]);
    let metadata = FileMetaData::read_from_in_protocol(&mut protocol)
        .map_err(|_error| CodecError::InvalidPageLayout)?;
    if protocol.remaining_len() != 0 || protocol.nesting != 0 {
        return Err(CodecError::InvalidPageLayout);
    }
    Ok((metadata, metadata_start))
}

#[allow(
    clippy::too_many_lines,
    reason = "the single pass keeps all cross-column footer and page accounting visible"
)]
fn validate_file_metadata(
    body: &[u8],
    metadata: &FileMetaData,
    metadata_start: usize,
    max_decoded_bytes: usize,
    allow_dictionary: bool,
) -> Result<(), CodecError> {
    let rows = checked_rows(metadata.num_rows)?;
    if rows > MAX_SECTION_ROWS {
        return Err(CodecError::TooManyRows {
            rows,
            max: MAX_SECTION_ROWS,
        });
    }
    if metadata.row_groups.len() > MAX_ROW_GROUPS {
        return Err(CodecError::TooManyRowGroups {
            groups: metadata.row_groups.len(),
            max: MAX_ROW_GROUPS,
        });
    }
    if metadata.schema.is_empty()
        || metadata.encryption_algorithm.is_some()
        || metadata.footer_signing_key_metadata.is_some()
    {
        return Err(CodecError::InvalidPageLayout);
    }

    let mut grouped_rows = 0_usize;
    let mut footer_work = 0_usize;
    let mut ranges = Vec::new();
    for group in &metadata.row_groups {
        let group_rows = checked_rows(group.num_rows)?;
        if group_rows > MAX_SECTION_ROWS {
            return Err(CodecError::TooManyRows {
                rows: group_rows,
                max: MAX_SECTION_ROWS,
            });
        }
        grouped_rows = grouped_rows
            .checked_add(group_rows)
            .ok_or(CodecError::InvalidPageLayout)?;
        if group.columns.is_empty() {
            return Err(CodecError::InvalidPageLayout);
        }
        for chunk in &group.columns {
            if chunk.file_path.is_some()
                || chunk.crypto_metadata.is_some()
                || chunk.encrypted_column_metadata.is_some()
            {
                return Err(CodecError::InvalidPageLayout);
            }
            let column = chunk
                .meta_data
                .as_ref()
                .ok_or(CodecError::InvalidPageLayout)?;
            let declared = checked_nonnegative(column.total_uncompressed_size)?;
            footer_work = add_work(footer_work, declared, max_decoded_bytes)?;
            let values = checked_nonnegative(column.num_values)?;
            if values > page_value_limit() {
                return Err(CodecError::InvalidPageLayout);
            }
            let compressed = checked_nonnegative(column.total_compressed_size)?;
            let data_start = checked_nonnegative(column.data_page_offset)?;
            let has_dictionary = column.dictionary_page_offset.is_some();
            if has_dictionary && !allow_dictionary {
                return Err(CodecError::DictionaryEncodingUnsupported);
            }
            let start = match column.dictionary_page_offset {
                Some(raw) => {
                    let dictionary_start = checked_nonnegative(raw)?;
                    if dictionary_start > data_start {
                        return Err(CodecError::InvalidPageLayout);
                    }
                    dictionary_start
                }
                None => data_start,
            };
            if compressed == 0 {
                if declared != 0
                    || values != 0
                    || has_dictionary
                    || start != data_start
                    || start < MAGIC.len()
                    || start > metadata_start
                {
                    return Err(CodecError::InvalidPageLayout);
                }
                continue;
            }
            let end = start
                .checked_add(compressed)
                .filter(|&end| {
                    start >= MAGIC.len()
                        && data_start >= start
                        && data_start <= end
                        && end <= metadata_start
                })
                .ok_or(CodecError::InvalidPageLayout)?;
            ranges.push((start, end, data_start, declared, values, has_dictionary));
        }
    }
    if grouped_rows != rows {
        return Err(CodecError::InvalidPageLayout);
    }

    ranges.sort_unstable_by_key(|&(start, _, _, _, _, _)| start);
    if ranges.windows(2).any(|pair| pair[0].1 > pair[1].0) {
        return Err(CodecError::InvalidPageLayout);
    }

    let mut page_work = 0_usize;
    let mut page_count = 0_usize;
    for (start, end, data_start, declared, values, has_dictionary) in ranges {
        let mut offset = start;
        let mut column_work = 0_usize;
        let mut data_values = 0_usize;
        let mut saw_dictionary = false;
        let mut saw_data = false;
        while offset < end {
            page_count = page_count
                .checked_add(1)
                .filter(|&count| count <= MAX_SECTION_ROWS)
                .ok_or(CodecError::InvalidPageLayout)?;
            let mut protocol = BoundedCompactInput::new(&body[offset..end]);
            let before = protocol.remaining_len();
            let header = PageHeader::read_from_in_protocol(&mut protocol)
                .map_err(|_error| CodecError::InvalidPageLayout)?;
            let header_len = before
                .checked_sub(protocol.remaining_len())
                .filter(|&len| len != 0 && protocol.nesting == 0)
                .ok_or(CodecError::InvalidPageLayout)?;
            let compressed = checked_nonnegative(i64::from(header.compressed_page_size))?;
            let uncompressed = checked_nonnegative(i64::from(header.uncompressed_page_size))?;
            match header.type_ {
                PageType::DICTIONARY_PAGE
                    if has_dictionary && !saw_dictionary && offset == start => {}
                PageType::DICTIONARY_PAGE => return Err(CodecError::InvalidPageLayout),
                PageType::DATA_PAGE | PageType::DATA_PAGE_V2 if !saw_data => {
                    if offset != data_start {
                        return Err(CodecError::InvalidPageLayout);
                    }
                    saw_data = true;
                }
                PageType::DATA_PAGE | PageType::DATA_PAGE_V2 => {}
                _ if offset < data_start => return Err(CodecError::InvalidPageLayout),
                _ => {}
            }
            let decoded =
                header_len
                    .checked_add(uncompressed)
                    .ok_or(CodecError::DecodedSectionTooLarge {
                        len: usize::MAX,
                        max: max_decoded_bytes,
                    })?;
            column_work =
                column_work
                    .checked_add(decoded)
                    .ok_or(CodecError::DecodedSectionTooLarge {
                        len: usize::MAX,
                        max: max_decoded_bytes,
                    })?;
            page_work = add_work(page_work, decoded, max_decoded_bytes)?;
            validate_page_header(
                &header,
                uncompressed,
                compressed,
                &mut data_values,
                &mut saw_dictionary,
            )?;
            offset = offset
                .checked_add(header_len)
                .and_then(|offset| offset.checked_add(compressed))
                .filter(|&offset| offset <= end)
                .ok_or(CodecError::InvalidPageLayout)?;
        }
        if offset != end
            || column_work != declared
            || data_values != values
            || saw_dictionary != has_dictionary
            || (values != 0 && !saw_data)
        {
            return Err(CodecError::InvalidPageLayout);
        }
    }
    if page_work != footer_work {
        return Err(CodecError::InvalidPageLayout);
    }
    Ok(())
}

fn validate_page_header(
    header: &PageHeader,
    uncompressed: usize,
    compressed: usize,
    data_values: &mut usize,
    saw_dictionary: &mut bool,
) -> Result<(), CodecError> {
    match header.type_ {
        PageType::DATA_PAGE => {
            let page = header
                .data_page_header
                .as_ref()
                .filter(|_| {
                    header.data_page_header_v2.is_none()
                        && header.dictionary_page_header.is_none()
                        && header.index_page_header.is_none()
                })
                .ok_or(CodecError::InvalidPageLayout)?;
            add_page_values(data_values, page.num_values)?;
        }
        PageType::DATA_PAGE_V2 => {
            let page = header
                .data_page_header_v2
                .as_ref()
                .filter(|_| {
                    header.data_page_header.is_none()
                        && header.dictionary_page_header.is_none()
                        && header.index_page_header.is_none()
                })
                .ok_or(CodecError::InvalidPageLayout)?;
            let values = checked_nonnegative(i64::from(page.num_values))?;
            let nulls = checked_nonnegative(i64::from(page.num_nulls))?;
            let rows = checked_nonnegative(i64::from(page.num_rows))?;
            let definition = checked_nonnegative(i64::from(page.definition_levels_byte_length))?;
            let repetition = checked_nonnegative(i64::from(page.repetition_levels_byte_length))?;
            if nulls > values || rows > MAX_SECTION_ROWS {
                return Err(CodecError::InvalidPageLayout);
            }
            let levels = definition
                .checked_add(repetition)
                .filter(|&len| len <= uncompressed && len <= compressed)
                .ok_or(CodecError::InvalidPageLayout)?;
            let _ = levels;
            add_page_values(data_values, page.num_values)?;
        }
        PageType::DICTIONARY_PAGE => {
            let page = header
                .dictionary_page_header
                .as_ref()
                .filter(|_| {
                    header.data_page_header.is_none()
                        && header.data_page_header_v2.is_none()
                        && header.index_page_header.is_none()
                })
                .ok_or(CodecError::InvalidPageLayout)?;
            let values = checked_nonnegative(i64::from(page.num_values))?;
            if *saw_dictionary || values > page_value_limit() {
                return Err(CodecError::InvalidPageLayout);
            }
            *saw_dictionary = true;
        }
        PageType::INDEX_PAGE => {
            if header.index_page_header.is_none()
                || header.data_page_header.is_some()
                || header.data_page_header_v2.is_some()
                || header.dictionary_page_header.is_some()
            {
                return Err(CodecError::InvalidPageLayout);
            }
        }
        _ => return Err(CodecError::InvalidPageLayout),
    }
    Ok(())
}

fn checked_rows(raw: i64) -> Result<usize, CodecError> {
    usize::try_from(raw).map_err(|_error| CodecError::InvalidRowCount { raw })
}

fn checked_nonnegative(raw: i64) -> Result<usize, CodecError> {
    usize::try_from(raw).map_err(|_error| CodecError::InvalidPageLayout)
}

const fn page_value_limit() -> usize {
    MAX_SECTION_ROWS + MAX_LIST_I32_VALUES_PER_SECTION
}

fn add_page_values(total: &mut usize, raw: i32) -> Result<(), CodecError> {
    let values = checked_nonnegative(i64::from(raw))?;
    *total = total
        .checked_add(values)
        .filter(|&count| count <= page_value_limit())
        .ok_or(CodecError::InvalidPageLayout)?;
    Ok(())
}

fn add_work(total: usize, add: usize, max: usize) -> Result<usize, CodecError> {
    let next = total
        .checked_add(add)
        .ok_or(CodecError::DecodedSectionTooLarge {
            len: usize::MAX,
            max,
        })?;
    if next > max {
        Err(CodecError::DecodedSectionTooLarge { len: next, max })
    } else {
        Ok(next)
    }
}

struct BoundedCompactInput<'a> {
    remaining: &'a [u8],
    last_field_id: i16,
    field_stack: [i16; MAX_THRIFT_NESTING],
    struct_depth: usize,
    nesting: usize,
    collection_items: usize,
    pending_bool: Option<bool>,
}

impl<'a> BoundedCompactInput<'a> {
    const fn new(remaining: &'a [u8]) -> Self {
        Self {
            remaining,
            last_field_id: 0,
            field_stack: [0; MAX_THRIFT_NESTING],
            struct_depth: 0,
            nesting: 0,
            collection_items: 0,
            pending_bool: None,
        }
    }

    const fn remaining_len(&self) -> usize {
        self.remaining.len()
    }

    fn enter(&mut self) -> thrift::Result<()> {
        if self.nesting >= MAX_THRIFT_NESTING {
            return Err(invalid_data("compact metadata nesting is too deep"));
        }
        self.nesting += 1;
        Ok(())
    }

    fn leave(&mut self) -> thrift::Result<()> {
        self.nesting = self
            .nesting
            .checked_sub(1)
            .ok_or_else(|| invalid_data("unbalanced compact metadata"))?;
        Ok(())
    }

    fn admit_items(&mut self, count: usize) -> thrift::Result<()> {
        if count > self.remaining.len() {
            return Err(invalid_data("collection exceeds remaining input"));
        }
        self.collection_items = self
            .collection_items
            .checked_add(count)
            .filter(|&items| items <= MAX_THRIFT_ITEMS)
            .ok_or_else(|| invalid_data("compact metadata has too many collection items"))?;
        Ok(())
    }

    fn read_vlq(&mut self) -> thrift::Result<u64> {
        let mut value = 0_u64;
        for index in 0..10_u32 {
            let byte = self.read_byte()?;
            let payload = u64::from(byte & 0x7f);
            if index == 9 && payload > 1 {
                return Err(invalid_data("compact integer exceeds u64"));
            }
            value |= payload << (index * 7);
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err(invalid_data("unterminated compact integer"))
    }

    fn read_zigzag(&mut self) -> thrift::Result<i64> {
        let value = self.read_vlq()?;
        let half = i64::try_from(value >> 1)
            .map_err(|_error| invalid_data("compact integer exceeds i64"))?;
        Ok(if value & 1 == 0 { half } else { !half })
    }

    fn read_collection(&mut self) -> thrift::Result<(TType, i32)> {
        self.enter()?;
        let header = self.read_byte()?;
        let element_type = compact_type(header & 0x0f)?;
        if element_type == TType::Stop {
            return Err(invalid_data("collection element type is stop"));
        }
        let inline = (header & 0xf0) >> 4;
        let count = if inline == 15 {
            self.read_vlq()?
        } else {
            u64::from(inline)
        };
        let count =
            usize::try_from(count).map_err(|_error| invalid_data("collection is too large"))?;
        self.admit_items(count)?;
        let count =
            i32::try_from(count).map_err(|_error| invalid_data("collection is too large"))?;
        Ok((element_type, count))
    }
}

impl TInputProtocol for BoundedCompactInput<'_> {
    fn read_message_begin(&mut self) -> thrift::Result<TMessageIdentifier> {
        Err(not_implemented(
            "messages are not valid in Parquet metadata",
        ))
    }

    fn read_message_end(&mut self) -> thrift::Result<()> {
        Err(not_implemented(
            "messages are not valid in Parquet metadata",
        ))
    }

    fn read_struct_begin(&mut self) -> thrift::Result<Option<TStructIdentifier>> {
        self.enter()?;
        if self.struct_depth >= self.field_stack.len() {
            return Err(invalid_data("compact metadata nesting is too deep"));
        }
        self.field_stack[self.struct_depth] = self.last_field_id;
        self.struct_depth += 1;
        self.last_field_id = 0;
        Ok(None)
    }

    fn read_struct_end(&mut self) -> thrift::Result<()> {
        self.struct_depth = self
            .struct_depth
            .checked_sub(1)
            .ok_or_else(|| invalid_data("unbalanced compact struct"))?;
        self.last_field_id = self.field_stack[self.struct_depth];
        self.leave()
    }

    fn read_field_begin(&mut self) -> thrift::Result<TFieldIdentifier> {
        if self.pending_bool.is_some() {
            return Err(invalid_data("compact boolean field was not consumed"));
        }
        let header = self.read_byte()?;
        let delta = i16::from((header & 0xf0) >> 4);
        let field_type = match header & 0x0f {
            1 => {
                self.pending_bool = Some(true);
                TType::Bool
            }
            2 => {
                self.pending_bool = Some(false);
                TType::Bool
            }
            value => compact_type(value)?,
        };
        if field_type == TType::Stop {
            return Ok(TFieldIdentifier {
                name: None,
                field_type,
                id: None,
            });
        }
        self.last_field_id = if delta == 0 {
            self.read_i16()?
        } else {
            self.last_field_id
                .checked_add(delta)
                .ok_or_else(|| invalid_data("compact field id overflow"))?
        };
        Ok(TFieldIdentifier {
            name: None,
            field_type,
            id: Some(self.last_field_id),
        })
    }

    fn read_field_end(&mut self) -> thrift::Result<()> {
        Ok(())
    }

    fn read_bool(&mut self) -> thrift::Result<bool> {
        if let Some(value) = self.pending_bool.take() {
            return Ok(value);
        }
        match self.read_byte()? {
            1 => Ok(true),
            2 => Ok(false),
            _ => Err(invalid_data("invalid compact boolean")),
        }
    }

    fn read_bytes(&mut self) -> thrift::Result<Vec<u8>> {
        let len = usize::try_from(self.read_vlq()?)
            .map_err(|_error| invalid_data("binary length exceeds usize"))?;
        let bytes = self
            .remaining
            .get(..len)
            .ok_or_else(|| end_of_file("binary field exceeds remaining input"))?;
        self.remaining = &self.remaining[len..];
        Ok(bytes.to_vec())
    }

    fn read_i8(&mut self) -> thrift::Result<i8> {
        Ok(i8::from_ne_bytes([self.read_byte()?]))
    }

    fn read_i16(&mut self) -> thrift::Result<i16> {
        i16::try_from(self.read_zigzag()?)
            .map_err(|_error| invalid_data("compact i16 is out of range"))
    }

    fn read_i32(&mut self) -> thrift::Result<i32> {
        i32::try_from(self.read_zigzag()?)
            .map_err(|_error| invalid_data("compact i32 is out of range"))
    }

    fn read_i64(&mut self) -> thrift::Result<i64> {
        self.read_zigzag()
    }

    fn read_double(&mut self) -> thrift::Result<f64> {
        let bytes: [u8; 8] = self
            .remaining
            .get(..8)
            .ok_or_else(|| end_of_file("double exceeds remaining input"))?
            .try_into()
            .map_err(|_error| invalid_data("double has an invalid width"))?;
        self.remaining = &self.remaining[8..];
        Ok(f64::from_le_bytes(bytes))
    }

    fn read_string(&mut self) -> thrift::Result<String> {
        String::from_utf8(self.read_bytes()?).map_err(Into::into)
    }

    fn read_list_begin(&mut self) -> thrift::Result<TListIdentifier> {
        let (element_type, size) = self.read_collection()?;
        Ok(TListIdentifier::new(element_type, size))
    }

    fn read_list_end(&mut self) -> thrift::Result<()> {
        self.leave()
    }

    fn read_set_begin(&mut self) -> thrift::Result<TSetIdentifier> {
        Err(not_implemented(
            "sets are not valid in supported Parquet metadata",
        ))
    }

    fn read_set_end(&mut self) -> thrift::Result<()> {
        Err(not_implemented(
            "sets are not valid in supported Parquet metadata",
        ))
    }

    fn read_map_begin(&mut self) -> thrift::Result<TMapIdentifier> {
        Err(not_implemented(
            "maps are not valid in supported Parquet metadata",
        ))
    }

    fn read_map_end(&mut self) -> thrift::Result<()> {
        Err(not_implemented(
            "maps are not valid in supported Parquet metadata",
        ))
    }

    fn read_byte(&mut self) -> thrift::Result<u8> {
        let byte = *self
            .remaining
            .first()
            .ok_or_else(|| end_of_file("compact metadata ended unexpectedly"))?;
        self.remaining = &self.remaining[1..];
        Ok(byte)
    }
}

fn compact_type(value: u8) -> thrift::Result<TType> {
    match value {
        0 => Ok(TType::Stop),
        1 | 2 => Ok(TType::Bool),
        3 => Ok(TType::I08),
        4 => Ok(TType::I16),
        5 => Ok(TType::I32),
        6 => Ok(TType::I64),
        7 => Ok(TType::Double),
        8 => Ok(TType::String),
        9 => Ok(TType::List),
        10 => Ok(TType::Set),
        11 => Ok(TType::Map),
        12 => Ok(TType::Struct),
        _ => Err(invalid_data("unknown compact field type")),
    }
}

fn invalid_data(message: &'static str) -> thrift::Error {
    thrift::Error::Protocol(thrift::ProtocolError {
        kind: thrift::ProtocolErrorKind::InvalidData,
        message: message.to_owned(),
    })
}

fn not_implemented(message: &'static str) -> thrift::Error {
    thrift::Error::Protocol(thrift::ProtocolError {
        kind: thrift::ProtocolErrorKind::NotImplemented,
        message: message.to_owned(),
    })
}

fn end_of_file(message: &'static str) -> thrift::Error {
    thrift::Error::Transport(thrift::TransportError {
        kind: thrift::TransportErrorKind::EndOfFile,
        message: message.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
    use parquet::thrift::{TCompactOutputProtocol, TSerializable};
    use thrift::protocol::{TInputProtocol, TOutputProtocol};

    use crate::bgwriter_checkpointer::BgwriterCheckpointer;
    use crate::{CodecError, Section, Ts, VerifiedSection, decode_any};

    use super::{
        BoundedCompactInput, MAGIC, MAX_THRIFT_NESTING, PageHeader, validate_parquet_decode_work,
    };
    use crate::MAX_DECODED_SECTION_BYTES;

    fn row() -> BgwriterCheckpointer {
        BgwriterCheckpointer {
            ts: Ts(42),
            checkpoints_timed: 10,
            checkpoints_req: 2,
            checkpoint_write_time: 1.0,
            checkpoint_sync_time: 2.0,
            buffers_checkpoint: 4_096,
            restartpoints_timed: None,
            restartpoints_req: None,
            restartpoints_done: None,
            buffers_clean: 512,
            maxwritten_clean: 3,
            buffers_backend: Some(128),
            buffers_backend_fsync: Some(0),
            buffers_alloc: 9_000,
            bgwriter_stats_reset: Ts(1),
            checkpointer_stats_reset: None,
        }
    }

    fn rewrite_first_page_header(mut body: Vec<u8>, edit: impl FnOnce(&mut PageHeader)) -> Vec<u8> {
        let builder = ParquetRecordBatchReaderBuilder::try_new(Bytes::from(body.clone()))
            .expect("read valid footer");
        let column = &builder.metadata().row_groups()[0].columns()[0];
        let start = usize::try_from(
            column
                .dictionary_page_offset()
                .unwrap_or_else(|| column.data_page_offset()),
        )
        .expect("positive column offset");
        let end =
            start + usize::try_from(column.compressed_size()).expect("positive compressed size");
        let mut input = BoundedCompactInput::new(&body[start..end]);
        let before = input.remaining_len();
        let mut header = PageHeader::read_from_in_protocol(&mut input).expect("read page header");
        let original_header_len = before - input.remaining_len();
        edit(&mut header);
        let mut encoded = Vec::new();
        {
            let mut output = TCompactOutputProtocol::new(&mut encoded);
            header
                .write_to_out_protocol(&mut output)
                .expect("encode changed header");
            output.flush().expect("flush header");
        }
        assert!(encoded.len() < end - start, "changed header fits chunk");
        let payload = body[start + original_header_len..end].to_vec();
        let keep = (end - start - encoded.len()).min(payload.len());
        encoded.extend_from_slice(&payload[..keep]);
        encoded.resize(end - start, 0);
        body[start..end].copy_from_slice(&encoded);
        body
    }

    #[test]
    fn valid_body_is_admitted_at_its_exact_footer_work() {
        let body = BgwriterCheckpointer::encode(&[row()]).expect("encode section");
        let builder = ParquetRecordBatchReaderBuilder::try_new(Bytes::from(body.clone()))
            .expect("read footer");
        let work = builder
            .metadata()
            .row_groups()
            .iter()
            .flat_map(parquet::file::metadata::RowGroupMetaData::columns)
            .map(|column| usize::try_from(column.uncompressed_size()).expect("positive size"))
            .sum::<usize>();
        validate_parquet_decode_work(&body, work).expect("exact work is admitted");
        assert!(matches!(
            validate_parquet_decode_work(&body, work - 1),
            Err(CodecError::DecodedSectionTooLarge { .. })
        ));
    }

    #[test]
    fn oversized_page_header_is_rejected_before_parquet_decode() {
        let body = BgwriterCheckpointer::encode(&[row()]).expect("encode section");
        let body = rewrite_first_page_header(body, |header| {
            header.uncompressed_page_size =
                i32::try_from(MAX_DECODED_SECTION_BYTES + 1).expect("cap fits i32");
        });
        let err = decode_any(
            BgwriterCheckpointer::CONTRACT.type_id.get(),
            VerifiedSection::for_test(Bytes::from(body)),
        )
        .expect_err("oversized page claim is rejected");
        assert!(matches!(
            err,
            CodecError::Section { source, .. }
                if matches!(*source, CodecError::DecodedSectionTooLarge { .. })
        ));
    }

    #[test]
    fn oversized_page_value_claim_is_rejected_before_decode() {
        let body = BgwriterCheckpointer::encode(&[row()]).expect("encode section");
        let body = rewrite_first_page_header(body, |header| {
            if let Some(dictionary) = header.dictionary_page_header.as_mut() {
                dictionary.num_values = i32::MAX;
            } else if let Some(data) = header.data_page_header.as_mut() {
                data.num_values = i32::MAX;
            } else if let Some(data) = header.data_page_header_v2.as_mut() {
                data.num_values = i32::MAX;
            } else {
                panic!("first page carries values");
            }
        });
        let err = decode_any(
            BgwriterCheckpointer::CONTRACT.type_id.get(),
            VerifiedSection::for_test(Bytes::from(body)),
        )
        .expect_err("oversized value claim is rejected");
        assert!(matches!(
            err,
            CodecError::Section { source, .. }
                if matches!(*source, CodecError::InvalidPageLayout)
        ));
    }

    #[test]
    fn oversized_footer_collection_is_rejected_without_a_builder() {
        let metadata = [0x15, 0x02, 0x19, 0xfc, 0x81, 0x80, 0x04];
        let mut body = MAGIC.to_vec();
        body.extend_from_slice(&metadata);
        body.extend_from_slice(
            &u32::try_from(metadata.len())
                .expect("small handcrafted footer")
                .to_le_bytes(),
        );
        body.extend_from_slice(MAGIC);
        assert!(matches!(
            validate_parquet_decode_work(&body, MAX_DECODED_SECTION_BYTES),
            Err(CodecError::InvalidPageLayout)
        ));
    }

    #[test]
    fn compact_reader_rejects_excessive_nesting() {
        let bytes = vec![0_u8; MAX_THRIFT_NESTING + 1];
        let mut input = BoundedCompactInput::new(&bytes);
        for _ in 0..MAX_THRIFT_NESTING {
            input.read_struct_begin().expect("within depth bound");
        }
        assert!(input.read_struct_begin().is_err());
    }
}

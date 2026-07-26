//! Independent physical and reader checks for a collector-produced compact PGM.

use std::collections::BTreeMap;
use std::fs::File;
use std::os::unix::fs::FileExt as _;
use std::path::Path;

use anyhow::{Context, Result, ensure};
use bytes::Bytes;
use kronika_format::{ENTRY_LEN, FORMAT_VERSION, MAGIC, META_LEN, TAIL_INDEX_LEN, crc32c};
use kronika_reader::PgmUnit;
use kronika_registry::{
    COMPACTION_ZSTD_LEVEL, Cell, DICT_BLOBS_TYPE_ID, DICT_STRINGS_TYPE_ID, registry,
};
use parquet::basic::{Compression, Encoding};
use parquet::file::reader::FileReader as _;
use parquet::file::serialized_reader::SerializedFileReader;

/// Verify the emitted bytes independently of the production seal verifier.
///
/// The check opens the file twice through the current reader and requires
/// byte-for-byte logical rows on both opens. It also walks every Parquet page
/// and the outer catalog, so a successful writer return alone cannot satisfy
/// the BDD contract.
#[allow(
    clippy::too_many_lines,
    reason = "one physical audit keeps the outer catalog, Parquet profile, dictionary, and reopen invariants together"
)]
pub(crate) fn assert_current_compact_pgm(path: &Path) -> Result<()> {
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let file_len = file
        .metadata()
        .with_context(|| format!("stat {}", path.display()))?
        .len();
    let mut leading_magic = [0_u8; 4];
    file.read_exact_at(&mut leading_magic, 0)
        .context("read leading PGM magic")?;
    ensure!(
        leading_magic == MAGIC,
        "leading magic {leading_magic:02x?} is not current {MAGIC:02x?}"
    );

    let unit = PgmUnit::open(file.try_clone().context("clone PGM file")?)
        .context("open current PGM reader")?;
    ensure!(
        unit.catalog().format_version == FORMAT_VERSION,
        "catalog format_version {} is not current {FORMAT_VERSION}",
        unit.catalog().format_version
    );
    ensure!(
        !unit.catalog().entries.is_empty(),
        "collector emitted an empty PGM catalog"
    );

    let catalog_len = unit
        .catalog()
        .entries
        .len()
        .checked_mul(ENTRY_LEN)
        .and_then(|entries| entries.checked_add(META_LEN))
        .context("catalog length overflow")?;
    let catalog_start = file_len
        .checked_sub(u64::try_from(catalog_len).context("catalog length exceeds u64")?)
        .and_then(|offset| offset.checked_sub(TAIL_INDEX_LEN as u64))
        .context("catalog does not fit in the PGM")?;
    let mut expected_offset = u64::try_from(MAGIC.len()).context("magic length exceeds u64")?;
    let mut previous_type = None;
    let mut first_rows = BTreeMap::new();

    let dictionary = unit
        .dictionary()
        .context("decode normalized PGM dictionary")?;
    for entry in &unit.catalog().entries {
        ensure!(entry.flags == 0, "type {} has nonzero flags", entry.type_id);
        ensure!(entry.rows > 0, "type {} has zero rows", entry.type_id);
        ensure!(entry.len > 0, "type {} has an empty body", entry.type_id);
        ensure!(
            previous_type.is_none_or(|previous| previous < entry.type_id),
            "catalog type ids are not unique and ascending at {}",
            entry.type_id
        );
        ensure!(
            entry.offset == expected_offset,
            "type {} begins at {}, expected packed offset {expected_offset}",
            entry.type_id,
            entry.offset
        );
        ensure!(
            registry()
                .iter()
                .any(|contract| contract.type_id.get() == entry.type_id)
                || matches!(entry.type_id, DICT_STRINGS_TYPE_ID | DICT_BLOBS_TYPE_ID),
            "catalog contains unknown type {}",
            entry.type_id
        );

        let len = usize::try_from(entry.len).context("section length exceeds usize")?;
        let mut body = vec![0_u8; len];
        file.read_exact_at(&mut body, entry.offset)
            .with_context(|| format!("read type {} body", entry.type_id))?;
        ensure!(
            crc32c(&body) == entry.crc32c,
            "type {} outer CRC differs",
            entry.type_id
        );
        assert_compact_parquet(Bytes::from(body), entry.rows, entry.type_id)?;

        if !matches!(entry.type_id, DICT_STRINGS_TYPE_ID | DICT_BLOBS_TYPE_ID) {
            let rows = unit
                .decode_rows(entry)
                .with_context(|| format!("decode type {} through current reader", entry.type_id))?;
            ensure!(
                rows.len() == entry.rows as usize,
                "type {} decoded {} rows, catalog declares {}",
                entry.type_id,
                rows.len(),
                entry.rows
            );
            for row in &rows {
                for cell in row.cells() {
                    if let Cell::StrId(id) = cell {
                        ensure!(
                            *id != 0 && dictionary.resolve(*id).is_some(),
                            "type {} references unresolved str_id {id}",
                            entry.type_id
                        );
                    }
                }
            }
            first_rows.insert(entry.type_id, rows);
        }

        expected_offset = entry
            .offset
            .checked_add(entry.len)
            .context("packed section end overflow")?;
        previous_type = Some(entry.type_id);
    }
    ensure!(
        expected_offset == catalog_start,
        "packed section bodies end at {expected_offset}, catalog starts at {catalog_start}"
    );

    let reopened = PgmUnit::open(File::open(path).context("reopen PGM for reader restart proof")?)
        .context("reopen current PGM reader")?;
    ensure!(
        reopened.catalog() == unit.catalog(),
        "reader restart changed the catalog"
    );
    for entry in &reopened.catalog().entries {
        if let Some(expected) = first_rows.get(&entry.type_id) {
            let actual = reopened
                .decode_rows(entry)
                .with_context(|| format!("decode type {} after reader restart", entry.type_id))?;
            ensure!(
                actual == *expected,
                "type {} rows changed across reader restart",
                entry.type_id
            );
        }
    }
    Ok(())
}

fn assert_compact_parquet(body: Bytes, expected_rows: u32, type_id: u32) -> Result<()> {
    ensure!(
        COMPACTION_ZSTD_LEVEL == 6,
        "production compact writer level changed from Zstd-6"
    );
    let reader = SerializedFileReader::new(body)
        .with_context(|| format!("open type {type_id} Parquet body"))?;
    ensure!(
        reader.num_row_groups() == 1,
        "type {type_id} has {} row groups",
        reader.num_row_groups()
    );
    let metadata = reader.metadata();
    let file_metadata = metadata.file_metadata();
    ensure!(
        file_metadata.num_rows() == i64::from(expected_rows),
        "type {type_id} Parquet rows {} differ from catalog {expected_rows}",
        file_metadata.num_rows()
    );
    ensure!(
        file_metadata.created_by() == Some(""),
        "type {type_id} carries created_by metadata"
    );
    ensure!(
        file_metadata.key_value_metadata().is_none_or(Vec::is_empty),
        "type {type_id} carries key-value metadata"
    );

    let row_group = reader
        .get_row_group(0)
        .with_context(|| format!("open type {type_id} row group"))?;
    for column_index in 0..row_group.num_columns() {
        let column = row_group.metadata().column(column_index);
        ensure!(
            matches!(column.compression(), Compression::ZSTD(_)),
            "type {type_id} column {column_index} is not Zstandard-compressed"
        );
        ensure!(
            column.encodings().contains(&Encoding::PLAIN)
                && !column.encodings().iter().any(|encoding| matches!(
                    encoding,
                    Encoding::PLAIN_DICTIONARY | Encoding::RLE_DICTIONARY
                )),
            "type {type_id} column {column_index} does not use only the PLAIN value profile"
        );
        ensure!(
            column.statistics().is_none()
                && column.column_index_length().is_none()
                && column.offset_index_length().is_none(),
            "type {type_id} column {column_index} carries statistics or page indexes"
        );

        let pages = row_group
            .get_column_page_reader(column_index)
            .with_context(|| format!("open type {type_id} column {column_index} pages"))?;
        let mut data_pages = 0_usize;
        for page in pages {
            let page = page.with_context(|| {
                format!("read type {type_id} column {column_index} Parquet page")
            })?;
            ensure!(
                !page.is_dictionary_page(),
                "type {type_id} column {column_index} has a dictionary page"
            );
            if page.is_data_page() {
                data_pages = data_pages
                    .checked_add(1)
                    .context("data page count overflow")?;
                ensure!(
                    page.encoding() == Encoding::PLAIN,
                    "type {type_id} column {column_index} data page is not PLAIN"
                );
            }
        }
        ensure!(
            data_pages == 1,
            "type {type_id} column {column_index} has {data_pages} data pages"
        );
    }
    Ok(())
}

use std::fs;

use kronika_layout::FileKind;

use super::*;

#[tokio::test]
async fn storage_counts_each_layout_file_once_and_reports_filesystem_headroom() {
    let (directory, _, _) = fixture_response("/readyz").await;
    let address = crate::test_layout::named_address("143000.pgm");
    let pgm_path = crate::test_layout::file_path(directory.path(), address, FileKind::Pgm);
    let ovf_path = crate::test_layout::file_path(directory.path(), address, FileKind::Ovf);
    fs::write(&ovf_path, [7_u8; 20]).expect("write overview fixture");
    let journal_path = crate::test_layout::write_empty_journal(directory.path());
    fs::write(directory.path().join("notes.bin"), [5_u8; 7]).expect("write foreign fixture");

    let pgm_bytes = fs::metadata(pgm_path).expect("PGM metadata").len();
    let journal_bytes = fs::metadata(journal_path).expect("journal metadata").len();
    let (status, body) = serve(directory.path(), "/v1/storage").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["used_bytes"]["pgm"], pgm_bytes);
    assert_eq!(body["used_bytes"]["ovf"], 20);
    assert_eq!(body["used_bytes"]["journal"], journal_bytes);
    assert_eq!(body["used_bytes"]["other"], 7);
    assert!(
        body["filesystem"]["total_bytes"]
            .as_u64()
            .is_some_and(|bytes| bytes > 0)
    );
    assert!(
        body["filesystem"]["available_bytes"]
            .as_u64()
            .is_some_and(|bytes| bytes > 0)
    );
    assert_eq!(body["retention"]["status"], "unknown");
    assert_eq!(body["retention"]["reason"], "producer_status_unavailable");
}

#[tokio::test]
async fn storage_rejects_all_query_parameters_before_inventory() {
    let (_directory, status, body) = fixture_response("/v1/storage?source=legacy").await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "unknown_query_parameter");
}

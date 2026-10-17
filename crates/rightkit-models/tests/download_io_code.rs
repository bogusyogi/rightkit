//! Disk failures from `download_resumable` are `ErrorCode::Io`, not `ErrorCode::Download`.
//! The destination's parent is a regular file, so the directory cannot be created and the call
//! ends before any request is made.

use rightkit_models::download::{
    download_resumable, download_resumable_capped, part_path_for, DownloadRequest,
};
use rightkit_models::http::ModelHttp;
use rightkit_models::ErrorCode;

#[test]
fn an_uncreatable_destination_directory_is_an_io_error() {
    let tmp = tempfile::tempdir().unwrap();
    let blocker = tmp.path().join("blocker");
    std::fs::write(&blocker, b"a file, not a directory").unwrap();
    let dest = blocker.join("sub").join("f.bin");
    let part = part_path_for(&dest);
    let client = ModelHttp::default();
    let request = || DownloadRequest {
        url: "https://models.example.org/f.bin",
        part_path: &part,
        dest_path: &dest,
        expected_sha256: None,
        authorization: None,
        cancelled: &|| false,
    };
    let err = download_resumable(&client, request(), |_, _| {}).unwrap_err();
    assert_eq!(err.code, ErrorCode::Io, "{err}");
    let err = download_resumable_capped(&client, request(), Some(1024), |_, _| {}).unwrap_err();
    assert_eq!(err.code, ErrorCode::Io, "{err}");
}

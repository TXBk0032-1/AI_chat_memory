use crate::error::AppError;
use crate::import_history::{CONVERSATIONS_JSON_TOO_LARGE, read_zip_entry_with_limit};
use std::io::{Cursor, Write};
use zip::{CompressionMethod, ZipArchive, ZipWriter, write::SimpleFileOptions};

#[test]
fn rejects_zip_entry_that_exceeds_actual_output_limit_with_forged_metadata() {
    let mut archive_bytes = Vec::new();
    {
        let mut writer = ZipWriter::new(Cursor::new(&mut archive_bytes));
        writer
            .start_file(
                "conversations.json",
                SimpleFileOptions::default().compression_method(CompressionMethod::Deflated),
            )
            .unwrap();
        writer.write_all(&vec![b'a'; 2 * 1024]).unwrap();
        writer.finish().unwrap();
    }

    let central_header = archive_bytes
        .windows(4)
        .rposition(|window| window == b"PK\x01\x02")
        .unwrap();
    archive_bytes[central_header + 24..central_header + 28].copy_from_slice(&1_u32.to_le_bytes());

    let mut archive = ZipArchive::new(Cursor::new(archive_bytes)).unwrap();
    let file = archive.by_name("conversations.json").unwrap();
    assert_eq!(file.size(), 1, "测试 ZIP 必须伪造较小的声明大小");

    assert!(matches!(
        read_zip_entry_with_limit(file, 1024),
        Err(AppError::InvalidData(message)) if message == CONVERSATIONS_JSON_TOO_LARGE
    ));
}

#[test]
fn accepts_zip_entry_at_actual_output_limit() {
    let content = read_zip_entry_with_limit(Cursor::new(b"[]"), 2).unwrap();
    assert_eq!(content, "[]");
}

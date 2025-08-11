//! Test: splice from file

#![allow(clippy::cast_possible_truncation)]

use std::io;

use tokio::fs as async_fs;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream as AsyncTcpStream;

#[tokio::test]
async fn test_splice_from_file_no_offset() -> io::Result<()> {
    let config = SpliceFromFileTestConfig::new(None, None, "test_splice_from_file_no_offset");
    run_splice_from_file_test(config).await
}

#[tokio::test]
async fn test_splice_from_file_with_offset_start() -> io::Result<()> {
    let config =
        SpliceFromFileTestConfig::new(Some(6), None, "test_splice_from_file_with_offset_start");
    run_splice_from_file_test(config).await
}

#[tokio::test]
async fn test_splice_from_file_with_offset_end() -> io::Result<()> {
    let config =
        SpliceFromFileTestConfig::new(None, Some(6), "test_splice_from_file_with_offset_end");
    run_splice_from_file_test(config).await
}

#[tokio::test]
async fn test_splice_from_file_with_offset_both() -> io::Result<()> {
    let config =
        SpliceFromFileTestConfig::new(Some(4), Some(9), "test_splice_from_file_with_offset_both");
    run_splice_from_file_test(config).await
}

// ======

const FILE_CONTENT: [u8; 12] = *b"Hello, File!";

async fn test_file(file_name: &str) -> io::Result<async_fs::File> {
    let mut file = async_fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .append(false)
        .open(&format!("/tmp/{file_name}.txt"))
        .await?;

    file.write_all(&FILE_CONTENT).await?;
    file.flush().await?;

    Ok(file)
}

async fn splice_from_file(
    w: &mut AsyncTcpStream,
    f_offset_start: Option<u64>,
    f_offset_end: Option<u64>,
    file_name: &str,
) -> io::Result<()> {
    let mut r = test_file(file_name).await?;
    let r_len = r.metadata().await?.len();

    assert_eq!(r_len, FILE_CONTENT.len() as u64);

    tokio_splice2::sendfile(&mut r, w, r_len, f_offset_start, f_offset_end).await?;

    Ok(())
}

/// Test configuration for `splice_from_file` tests
struct SpliceFromFileTestConfig {
    f_offset_start: Option<u64>,
    f_offset_end: Option<u64>,
    file_name: &'static str,
}

impl SpliceFromFileTestConfig {
    fn new(
        f_offset_start: Option<u64>,
        f_offset_end: Option<u64>,
        file_name: &'static str,
    ) -> Self {
        Self {
            f_offset_start,
            f_offset_end,
            file_name,
        }
    }

    fn expected_data(&self) -> &[u8] {
        let start = self.f_offset_start.unwrap_or(0) as usize;
        let end = self.f_offset_end.unwrap_or(FILE_CONTENT.len() as u64) as usize;
        &FILE_CONTENT[start..end]
    }

    fn expected_length(&self) -> usize {
        self.expected_data().len()
    }
}

/// Common test runner for `splice_from_file` tests
async fn run_splice_from_file_test(config: SpliceFromFileTestConfig) -> io::Result<()> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let listen_at = listener.local_addr()?;

    let handle = tokio::spawn(async move {
        if let Ok((mut stream, _)) = listener.accept().await {
            splice_from_file(
                &mut stream,
                config.f_offset_start,
                config.f_offset_end,
                config.file_name,
            )
            .await
            .unwrap();
        }
    });

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let mut client = AsyncTcpStream::connect(listen_at).await?;
    let expected_data = config.expected_data().to_vec();
    let expected_length = config.expected_length();

    let client_handle = tokio::spawn(async move {
        let mut buf = vec![0; expected_length];
        client.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, &expected_data);
    });

    tokio::try_join!(handle, client_handle)?;

    Ok(())
}

//! Test: splice from file

#![allow(clippy::cast_possible_truncation)]

use std::io;
use std::sync::OnceLock;

use rand::RngCore;
use tokio::fs as async_fs;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream as AsyncTcpStream;

#[tokio::test]
async fn test_splice_to_file_no_offset() -> io::Result<()> {
    let config = SpliceToFileTestConfig::new(None, None, false, "test_splice_to_file_no_offset");
    run_splice_to_file_test(config).await
}

#[tokio::test]
async fn test_splice_to_file_with_offset_start_not_preserve_content_out_of_range() -> io::Result<()>
{
    let config = SpliceToFileTestConfig::new(
        Some(6),
        None,
        false,
        "test_splice_to_file_with_offset_start_not_preserve_content_out_of_range",
    );
    run_splice_to_file_test(config).await
}

#[tokio::test]
async fn test_splice_to_file_with_offset_end_not_preserve_content_out_of_range() -> io::Result<()> {
    let config = SpliceToFileTestConfig::new(
        None,
        Some(6),
        false,
        "test_splice_to_file_with_offset_end_not_preserve_content_out_of_range",
    );
    run_splice_to_file_test(config).await
}

#[tokio::test]
async fn test_splice_to_file_with_offset_both_not_preserve_content_out_of_range() -> io::Result<()>
{
    let config = SpliceToFileTestConfig::new(
        Some(4),
        Some(9),
        false,
        "test_splice_to_file_with_offset_both_not_preserve_content_out_of_range",
    );
    run_splice_to_file_test(config).await
}

// =========

const FILE_CONTENT: [u8; 12] = *b"Hello, File!";

static NEW_FILE_CONTENT: OnceLock<[u8; 128]> = OnceLock::new();

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

async fn splice_to_file(
    r: &mut AsyncTcpStream,
    mut f_offset_start: Option<u64>,
    mut f_offset_end: Option<u64>,
    preserve_content_out_of_range: bool,
    file_name: &str,
) -> io::Result<()> {
    let mut w = test_file(file_name).await?;
    let mut w_len = w.metadata().await?.len();
    let original_f_offset_start = f_offset_start.unwrap_or(0);

    assert_eq!(w_len, FILE_CONTENT.len() as u64);

    // The caller should handle whether to truncate or extend the file / only adjust
    // the file ending offset and keep the starting offset, etc.
    // Here: keep the starting offset; if preserve_content_out_of_range then keep
    // the ending offset if less than file length, extend the file if greater
    // than file length; if !preserve_content_out_of_range then just adjust the file
    // offset to match the file length, set_len when not equal.
    if preserve_content_out_of_range {
        // The f_offset_start should be None
        if f_offset_start.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "f_offset_start should be None when preserve_content_out_of_range is true",
            ));
        }

        if let Some(f_offset_end) = f_offset_end {
            if f_offset_end > w_len {
                w.set_len(f_offset_end).await?;
            } else if f_offset_end == 0 {
                return Ok(());
            } else {
                // ...
            }
        }
    } else {
        let new_len = f_offset_end
            .unwrap_or(w_len)
            .saturating_sub(f_offset_start.unwrap_or(0));

        if new_len == 0 {
            return Ok(());
        }

        w_len = new_len;
        w.set_len(new_len).await?;

        if new_len == 0 {
            return Ok(());
        }

        f_offset_start = None;
        f_offset_end = None;
    }

    tokio_splice2::SpliceIoCtx::prepare_writing_file(w_len, f_offset_start, f_offset_end)?
        .into_io()
        .execute(r, &mut w)
        .await;

    let mut buf = vec![0; w_len as usize];
    // println!("w_len: {w_len}");

    // Do no reuse the file handle, since its offset is not at the start of file.
    async_fs::OpenOptions::new()
        .read(true)
        .open(&format!("/tmp/{file_name}.txt"))
        .await?
        .read_exact(&mut buf)
        .await?;

    assert_eq!(
        buf,
        &NEW_FILE_CONTENT.get_or_init(init_new_file_content)
            [original_f_offset_start as usize..(original_f_offset_start + w_len) as usize]
    );

    Ok(())
}

/// Test configuration for `splice_to_file` tests
struct SpliceToFileTestConfig {
    f_offset_start: Option<u64>,
    f_offset_end: Option<u64>,
    preserve_content_out_of_range: bool,
    test_name: &'static str,
}

impl SpliceToFileTestConfig {
    fn new(
        f_offset_start: Option<u64>,
        f_offset_end: Option<u64>,
        preserve_content_out_of_range: bool,
        test_name: &'static str,
    ) -> Self {
        Self {
            f_offset_start,
            f_offset_end,
            preserve_content_out_of_range,
            test_name,
        }
    }

    fn data_to_write(&self) -> &[u8] {
        let start = self.f_offset_start.unwrap_or(0) as usize;
        let end = self.f_offset_end.unwrap_or(FILE_CONTENT.len() as u64) as usize;
        let new_file_content = NEW_FILE_CONTENT.get_or_init(init_new_file_content);

        if self.preserve_content_out_of_range {
            &new_file_content[start..]
        } else {
            &new_file_content[start..end]
        }
    }
}

/// Common test runner for `splice_to_file` tests
async fn run_splice_to_file_test(config: SpliceToFileTestConfig) -> io::Result<()> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let listen_at = listener.local_addr()?;

    let handle = tokio::spawn(async move {
        if let Ok((mut stream, _)) = listener.accept().await {
            splice_to_file(
                &mut stream,
                config.f_offset_start,
                config.f_offset_end,
                config.preserve_content_out_of_range,
                config.test_name,
            )
            .await
            .unwrap();
        }
    });

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let mut client = AsyncTcpStream::connect(listen_at).await?;
    let data_to_write = config.data_to_write().to_vec();

    let client_handle = tokio::spawn(async move {
        client.write_all(&data_to_write).await.unwrap();
    });

    tokio::try_join!(handle, client_handle)?;

    Ok(())
}

fn init_new_file_content() -> [u8; 128] {
    let mut content = [0; 128];
    content[..FILE_CONTENT.len()].copy_from_slice(b"Hell, oFile!");
    rand::rng().fill_bytes(&mut content[FILE_CONTENT.len()..]);
    content
}

//! Uni-test: Splice from file

use std::io;

use tokio::fs as async_fs;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream as AsyncTcpStream;

const FILE_CONTENT: [u8; 12] = *b"Hello, File!";

async fn test_file() -> io::Result<async_fs::File> {
    let mut file = async_fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .append(false)
        .open("/tmp/test_file.txt")
        .await?;

    file.write_all(&FILE_CONTENT).await?;
    file.flush().await?;

    Ok(file)
}

async fn splice_from_file(
    w: &mut AsyncTcpStream,
    f_offset_start: Option<u64>,
    f_offset_end: Option<u64>,
) -> io::Result<()> {
    let mut r = test_file().await?;
    let r_len = r.metadata().await?.len();

    assert_eq!(r_len, FILE_CONTENT.len() as u64);

    tokio_splice2::copy_from_file(&mut r, w, r_len, f_offset_start, f_offset_end).await?;

    Ok(())
}

#[tokio::test]
async fn test_splice_from_file_no_offset() -> io::Result<()> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let listen_at = listener.local_addr()?;

    let handle = tokio::spawn(async move {
        if let Ok((mut stream, _)) = listener.accept().await {
            splice_from_file(&mut stream, None, None).await.unwrap();
        }
    });

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let mut client = AsyncTcpStream::connect(listen_at).await?;

    let client_handle = tokio::spawn(async move {
        let mut buf = vec![0; FILE_CONTENT.len()];
        client.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, &FILE_CONTENT);
    });

    tokio::try_join!(handle, client_handle)?;

    Ok(())
}

#[tokio::test]
async fn test_splice_from_file_with_offset_start() -> io::Result<()> {
    const TEST_OFFSET: u64 = 6;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let listen_at = listener.local_addr()?;

    let handle = tokio::spawn(async move {
        if let Ok((mut stream, _)) = listener.accept().await {
            splice_from_file(&mut stream, Some(TEST_OFFSET), None)
                .await
                .unwrap();
        }
    });

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let mut client = AsyncTcpStream::connect(listen_at).await?;

    let client_handle = tokio::spawn(async move {
        let mut buf = vec![0; FILE_CONTENT.len() - TEST_OFFSET as usize];
        client.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, &FILE_CONTENT[TEST_OFFSET as usize..]);
    });

    tokio::try_join!(handle, client_handle)?;

    Ok(())
}

#[tokio::test]
async fn test_splice_from_file_with_offset_end() -> io::Result<()> {
    const TEST_OFFSET: u64 = 6;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let listen_at = listener.local_addr()?;

    let handle = tokio::spawn(async move {
        if let Ok((mut stream, _)) = listener.accept().await {
            splice_from_file(&mut stream, None, Some(TEST_OFFSET))
                .await
                .unwrap();
        }
    });

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let mut client = AsyncTcpStream::connect(listen_at).await?;

    let client_handle = tokio::spawn(async move {
        let mut buf = vec![0; TEST_OFFSET as usize];
        client.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, &FILE_CONTENT[..TEST_OFFSET as usize]);
    });

    tokio::try_join!(handle, client_handle)?;

    Ok(())
}

#[tokio::test]
async fn test_splice_from_file_with_offset_both() -> io::Result<()> {
    const TEST_OFFSET_FROM: u64 = 4;
    const TEST_OFFSET_TO: u64 = 9;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let listen_at = listener.local_addr()?;

    let handle = tokio::spawn(async move {
        if let Ok((mut stream, _)) = listener.accept().await {
            splice_from_file(&mut stream, Some(TEST_OFFSET_FROM), Some(TEST_OFFSET_TO))
                .await
                .unwrap();
        }
    });

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let mut client = AsyncTcpStream::connect(listen_at).await?;

    let client_handle = tokio::spawn(async move {
        let mut buf = vec![0; (TEST_OFFSET_TO - TEST_OFFSET_FROM) as usize];
        client.read_exact(&mut buf).await.unwrap();
        assert_eq!(
            &buf,
            &FILE_CONTENT[TEST_OFFSET_FROM as usize..TEST_OFFSET_TO as usize]
        );
    });

    tokio::try_join!(handle, client_handle)?;

    Ok(())
}

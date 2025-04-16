//! Example: simple L4 proxy

use std::{env, io};

use tokio::net::{TcpListener, TcpStream};

#[tokio::main(flavor = "current_thread")]
async fn main() -> io::Result<()> {
    println!("PID is {}", std::process::id());

    tokio::select! {
        res = serve() => {
            if let Err(err) = res {
                eprintln!("Serve failed {err}");
            }
        }
        _ = tokio::signal::ctrl_c() => {
            println!("Received Ctrl + C, shutting down");
        }
    }

    Ok(())
}

async fn serve() -> io::Result<()> {
    let listener = TcpListener::bind("127.0.0.1:8989").await?;

    loop {
        let (incoming, remote_addr) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(e) if e.kind() == io::ErrorKind::ConnectionAborted => {
                println!("Connection aborted.");
                continue;
            }
            Err(e) => {
                eprintln!("Failed to accept: {:#?}", e);
                break Err(e);
            }
        };

        println!("Process incoming connection from {remote_addr}");

        tokio::spawn(forwarding(incoming));
    }
}

async fn forwarding(mut stream1: TcpStream) -> io::Result<()> {
    let mut stream2 = TcpStream::connect(
        env::var("EXAMPLE_REMOTE_ADDR").unwrap_or_else(|_| "4.ipw.cn:80".to_string()),
    )
    .await
    .inspect_err(|e| {
        eprintln!("Failed to connect to remote server: {e}");
    })?;

    tokio_splice2::copy_bidirectional(&mut stream1, &mut stream2)
        .await
        .inspect(|(r, w)| {
            println!(
                "Forwarded {r} bytes from stream1 to stream2, {w} bytes from stream2 to stream1"
            );
        })
        .inspect_err(|e| {
            eprintln!("Failed to copy data: {e}");
        })?;
    Ok(())
}

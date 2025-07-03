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
    let listener = TcpListener::bind(
        env::var("EXAMPLE_LISTEN_ADDR").unwrap_or_else(|_| "0.0.0.0:5201".to_string()),
    )
    .await?;

    loop {
        let (incoming, remote_addr) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(e) if e.kind() == io::ErrorKind::ConnectionAborted => {
                println!("Connection aborted.");
                continue;
            }
            Err(e) => {
                eprintln!("Failed to accept: {e:#?}");
                break Err(e);
            }
        };

        println!("Process incoming connection from {remote_addr}");

        tokio::spawn(forwarding(incoming));
    }
}

async fn forwarding(mut stream1: TcpStream) -> io::Result<()> {
    let stream2 = TcpStream::connect(
        env::var("EXAMPLE_REMOTE_ADDR").unwrap_or_else(|_| "127.0.0.1:5202".to_string()),
    )
    .await;

    let mut stream2 = match stream2 {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Failed to connect to remote server: {e}");
            return Err(e);
        }
    };

    // let result = tokio_splice::zero_copy_bidirectional(&mut stream1, &mut
    // stream2).await;
    let instant = std::time::Instant::now();
    let result = tokio_splice2::copy_bidirectional(&mut stream1, &mut stream2).await;
    // let result = realm_io::bidi_zero_copy(&mut stream1, &mut stream2).await;

    match result {
        Ok(traffic) => {
            let total = traffic.sum();
            let cost = instant.elapsed();
            println!(
                "Forwarded traffic: {traffic:?}, total: {}, time: {:.2}s, avg: {}",
                human_format_next::Formatter::BINARY
                    .with_custom_unit("B")
                    .with_decimals::<4>()
                    .format(total as f64),
                cost.as_secs_f64(),
                human_format_next::Formatter::BINARY
                    .with_custom_unit("B/s")
                    .with_decimals::<4>()
                    .format(total as f64 / cost.as_secs_f64())
            );
            Ok(())
        }
        Err(e) => {
            eprintln!("Failed to copy data: {e}");
            Err(e)
        }
    }
}

use std::io;
use std::mem;
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use tokio_util::net::{ArcTcpStream, RefTcpStream};

#[tokio::test]
async fn shared_tcp_stream_concurrency() -> io::Result<()> {
    // Create two connected sockets, A and B.
    let listener = TcpListener::bind("localhost:0").await?;
    let a = Arc::new(TcpStream::connect(listener.local_addr()?).await?);
    let b = Arc::new(listener.accept().await?.0);

    // Each end will send 256 bytes in arbitrary order to the other side.
    // Side A will use `RefTcpStream`, side B will use `ArcTcpStream`.
    let tasks: Vec<_> = (0..=255)
        .map(|byte| {
            let a_read = tokio::spawn({
                let a = Arc::clone(&a);
                async move { RefTcpStream::new(&a).read_u8().await }
            });

            let a_write = tokio::spawn({
                let a = Arc::clone(&a);
                async move { RefTcpStream::new(&a).write_u8(byte).await }
            });

            let b_read = tokio::spawn({
                let b = Arc::clone(&b);
                async move { ArcTcpStream::new(b).read_u8().await }
            });

            let b_write = tokio::spawn({
                let b = Arc::clone(&b);
                async move { ArcTcpStream::new(b).write_u8(byte).await }
            });

            (a_read, a_write, b_read, b_write)
        })
        .collect();

    let mut a_bytes = [false; 256];
    let mut b_bytes = [false; 256];

    for (a_read, a_write, b_read, b_write) in tasks {
        assert!(!mem::replace(
            &mut a_bytes[usize::from(a_read.await.unwrap()?)],
            true
        ));
        a_write.await.unwrap()?;
        assert!(!mem::replace(
            &mut b_bytes[usize::from(b_read.await.unwrap()?)],
            true
        ));
        b_write.await.unwrap()?;
    }

    assert!(a_bytes.iter().all(|&received_byte| received_byte));
    assert!(a_bytes.iter().all(|&received_byte| received_byte));

    Ok(())
}

#[tokio::test]
async fn shared_tcp_stream_reuse() -> io::Result<()> {
    // Create two connected sockets, A and B.
    let listener = TcpListener::bind("localhost:0").await?;
    let a = Arc::new(TcpStream::connect(listener.local_addr()?).await?);
    let b = Arc::new(listener.accept().await?.0);

    // Write 1024 16-bit integers to socket A.
    let mut ref_a = RefTcpStream::new(&a);
    for number in 0..1024 {
        ref_a.write_u16(number * 4).await?;
    }
    drop(ref_a);

    // Read the 1024 16-bit integers from socket B.
    let mut ref_b = RefTcpStream::new(&b);
    for number in 0..1024 {
        assert_eq!(ref_b.read_u16().await?, number * 4);
    }
    drop(ref_b);

    // Write 1024 16-bit integers to socket B.
    let mut arc_b = ArcTcpStream::new(b);
    for number in 0..1024 {
        arc_b.write_i16_le(number * 3).await?;
    }

    // Read the 1024 16-bit integers from socket A.
    let mut arc_a = ArcTcpStream::new(a);
    for number in 0..1024 {
        assert_eq!(arc_a.read_i16_le().await?, number * 3);
    }

    Ok(())
}

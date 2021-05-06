use std::borrow::Borrow;
use std::io::{self, IoSlice};
use std::ops::Deref;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use futures_core::ready;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;

use crate::sync::ReusableBoxFuture;

/// An [`Arc`]`<`[`TcpStream`]`>` that implements [`AsyncRead`] and [`AsyncWrite`].
///
/// [`AsyncRead`] and [`AsyncWrite`] are not implemented for `&TcpStream`, meaning that concurrent
/// reads and writes cannot be performed through those traits. To support the common case of a single
/// read concurrently with a single write, Tokio provides the [`TcpStream::split`] and
/// [`TcpStream::into_split`] APIs, and for multiple concurrent reads/writes you can use this type.
///
/// Additionally, the [`poll_readable`] and [`poll_writable`] methods are provided as poll-based
/// analogues to [`TcpStream::readable`] and [`TcpStream::writable`].
///
/// Like [`Arc`], this type can be cheaply cloned.
///
/// [`poll_readable`]: ArcTcpStream::poll_readable
/// [`poll_writable`]: ArcTcpStream::poll_writable
#[derive(Debug)]
pub struct ArcTcpStream {
    inner: Arc<TcpStream>,
    /// `Option` is used to avoid allocating when one half of the TCP stream is not used.
    readable: Option<ReusableBoxFuture<'static, (io::Result<()>, Arc<TcpStream>)>>,
    writable: Option<ReusableBoxFuture<'static, (io::Result<()>, Arc<TcpStream>)>>,
}

async fn readable(stream: Arc<TcpStream>) -> (io::Result<()>, Arc<TcpStream>) {
    (stream.readable().await, stream)
}
async fn writable(stream: Arc<TcpStream>) -> (io::Result<()>, Arc<TcpStream>) {
    (stream.writable().await, stream)
}

impl ArcTcpStream {
    /// Creates a new `ArcTcpStream` from its inner [`Arc`]`<`[`TcpStream`]`>`.
    #[must_use]
    pub fn new(inner: Arc<TcpStream>) -> Self {
        Self {
            readable: None,
            writable: None,
            inner,
        }
    }

    /// Returns a clone of the inner [`TcpStream`].
    #[must_use]
    pub fn clone_inner(&self) -> Arc<TcpStream> {
        self.inner.clone()
    }

    /// Destructures the `ArcTcpStream`, returning its inner [`Arc`]`<`[`TcpStream`]`>`.
    #[must_use]
    pub fn into_inner(self) -> Arc<TcpStream> {
        self.inner
    }

    /// Polls for read readiness.
    ///
    /// If the TCP stream is not currently ready for reading, this method will store a clone of the
    /// [`Waker`] from the provided [`Context`]. When the TCP stream becomes ready for reading,
    /// [`Waker::wake`] will be called on the waker.
    ///
    /// Unlike [`TcpStream::poll_read_ready`], calling this from multiple `ArcTcpStream` instances
    /// will not cause wakeups to be lost.
    ///
    /// # Return value
    ///
    /// This function returns:
    ///
    /// * [`Poll::Pending`] if the TCP stream is not ready for reading.
    /// * [`Poll::Ready`]`(`[`Ok`]`(()))` if the TCP stream is ready for reading.
    /// * [`Poll::Ready`]`(`[`Err`]`(e))` if an error is encountered.
    ///
    /// # Errors
    ///
    /// This function may return any standard I/O error except [`WouldBlock`].
    ///
    /// [`Waker::wake`]: std::task::Waker::wake
    /// [`Waker`]: std::task::Waker
    /// [`WouldBlock`]: io::ErrorKind::WouldBlock.
    pub fn poll_readable(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let inner = &self.inner;
        let fut = self
            .readable
            .get_or_insert_with(|| ReusableBoxFuture::new(readable(Arc::clone(inner))));
        let (res, inner) = ready!(fut.poll(cx));
        fut.set(readable(inner));
        Poll::Ready(res)
    }

    /// Polls for write readiness.
    ///
    /// If the TCP stream is not current ready for writing, this method will store a clone of the
    /// [`Waker`] from the provided [`Context`]. When the TCP stream becomes ready for writing,
    /// [`Waker::wake`] will be called on the waker.
    ///
    /// Unlike [`TcpStream::poll_write_ready`], calling this from multiple `ArcTcpStream` instances
    /// will not cause wakeups to be lost.
    ///
    /// # Return value
    ///
    /// This function returns:
    ///
    /// * [`Poll::Pending`] if the TCP stream is not ready for writing.
    /// * [`Poll::Ready`]`(`[`Ok`]`(()))` if the TCP stream is ready for writing.
    /// * [`Poll::Ready`]`(`[`Err`]`(e))` if an error is encountered.
    ///
    /// # Errors
    ///
    /// This function may return any standard I/O error except [`WouldBlock`].
    ///
    /// [`Waker::wake`]: std::task::Waker::wake
    /// [`Waker`]: std::task::Waker
    /// [`WouldBlock`]: io::ErrorKind::WouldBlock.
    pub fn poll_writable(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let inner = &self.inner;
        let fut = self
            .writable
            .get_or_insert_with(|| ReusableBoxFuture::new(writable(Arc::clone(inner))));
        let (res, inner) = ready!(fut.poll(cx));
        fut.set(writable(inner));
        Poll::Ready(res)
    }
}

impl Deref for ArcTcpStream {
    type Target = TcpStream;
    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl Clone for ArcTcpStream {
    #[inline]
    fn clone(&self) -> Self {
        Self::new(self.clone_inner())
    }
}

impl AsyncRead for ArcTcpStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        loop {
            ready!(self.poll_readable(cx))?;
            match self.inner.try_read(buf.initialize_unfilled()) {
                Ok(res) => {
                    buf.advance(res);
                    break Poll::Ready(Ok(()));
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                Err(e) => break Poll::Ready(Err(e)),
            }
        }
    }
}
impl AsyncWrite for ArcTcpStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        loop {
            ready!(self.poll_writable(cx))?;
            match self.inner.try_write(buf) {
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                res => break Poll::Ready(res),
            }
        }
    }
    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        loop {
            ready!(self.poll_writable(cx))?;
            match self.inner.try_write_vectored(bufs) {
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                res => break Poll::Ready(res),
            }
        }
    }
    fn is_write_vectored(&self) -> bool {
        true
    }
    #[inline]
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        // TCP flush is a no-op
        Poll::Ready(Ok(()))
    }
    #[inline]
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        // Don't shutdown the TcpStream as we don't have ownership.
        Poll::Ready(Ok(()))
    }
}

impl AsRef<TcpStream> for ArcTcpStream {
    #[inline]
    fn as_ref(&self) -> &TcpStream {
        &self.inner
    }
}
impl Borrow<TcpStream> for ArcTcpStream {
    #[inline]
    fn borrow(&self) -> &TcpStream {
        &self.inner
    }
}

impl From<Arc<TcpStream>> for ArcTcpStream {
    #[inline]
    fn from(inner: Arc<TcpStream>) -> Self {
        Self::new(inner)
    }
}

#[cfg(unix)]
mod sys {
    use super::ArcTcpStream;
    use std::os::unix::io::{AsRawFd, RawFd};

    impl AsRawFd for ArcTcpStream {
        #[inline]
        fn as_raw_fd(&self) -> RawFd {
            self.inner.as_raw_fd()
        }
    }
}

#[cfg(windows)]
mod sys {
    use super::ArcTcpStream;
    use std::os::windows::io::{AsRawSocket, RawSocket};

    impl AsRawSocket for ArcTcpStream {
        #[inline]
        fn as_raw_socket(&self) -> RawSocket {
            self.inner.as_raw_socket()
        }
    }
}

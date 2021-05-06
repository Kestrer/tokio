//! Networking utility types.

mod tcp;
pub use tcp::arc_stream::ArcTcpStream;
pub use tcp::ref_stream::RefTcpStream;

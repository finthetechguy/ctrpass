//! The transport that carries exchange objects over a link.

pub mod frame;
pub mod session;

pub use frame::Segment;
pub use session::Session;

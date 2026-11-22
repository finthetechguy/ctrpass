//! StreetPass with Nintendo 3DS consoles.

pub mod cec;
pub mod exchange;
pub mod identity;
pub mod keys;
pub mod link;
pub mod mii;
pub mod plaza;
pub mod radio;
pub mod scan;
#[cfg(test)]
mod sim;
pub mod store;
pub mod streetpass;
pub mod wifi;
pub mod yy;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("key file: {0}")]
    Keys(String),
    #[error("{0}")]
    Format(String),
    #[error("authentication failed")]
    Authentication,
    #[error("{0}")]
    Busy(String),
    #[error("no room: {0}")]
    NoRoom(String),
    #[error("title {0:08X} already has a different key")]
    KeyConflict(u32),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

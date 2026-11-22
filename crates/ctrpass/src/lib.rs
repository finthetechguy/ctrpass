//! StreetPass with Nintendo 3DS consoles.

pub mod keys;
pub mod radio;
pub mod streetpass;
pub mod wifi;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("key file: {0}")]
    Keys(String),
    #[error("{0}")]
    Format(String),
    #[error("authentication failed")]
    Authentication,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

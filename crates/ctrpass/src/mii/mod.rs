//! Mii records and the protection applied to them inside messages.

mod record;
mod wrap;

pub use record::{FIELDS, Field, Issue, NAME_LEN, RECORD_SIZE, Record, STORED_SIZE, crc16};
pub use wrap::{WRAPPED_SIZE, unwrap, wrap};

//! glft core: identity, networking, manifests and verified transfers.
//!
//! This crate is shared by the terminal program now and the Android app later,
//! so it holds no terminal or UI code. Everything it does is reported through
//! [`events::Event`]s, which are written as plain sentences.

pub mod devices;
pub mod error;
pub mod help;
pub mod route;
pub mod events;
pub mod identity;
pub mod manifest;
pub mod net;
pub mod pairing;
pub mod paths;
pub mod proto;
pub mod resume;
pub mod storage;
pub mod transfer;

pub use error::{Error, ErrorKind};
pub use events::{Event, EventSink};

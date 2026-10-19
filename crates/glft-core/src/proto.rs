//! Wire messages.
//!
//! The sender opens one control stream and sends an [`Offer`]; the receiver answers
//! with an [`Answer`]. If accepted, the receiver pulls each file by opening a stream
//! and sending a [`FileRequest`]; the sender answers with BLAKE3 verified-stream data
//! for the requested chunks. When every file is in place, the receiver sends a
//! [`Done`] on the control stream.
//!
//! Each message is a 4-byte big-endian length followed by postcard bytes.

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::io::{AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::{
    error::{Error, ErrorKind},
    manifest::Manifest,
};

/// Large enough for a manifest of many thousands of files.
const MAX_MESSAGE: u32 = 32 * 1024 * 1024;

#[derive(Debug, Serialize, Deserialize)]
pub struct Offer {
    /// The sender's device name, shown to the receiver.
    pub device_name: String,
    pub manifest: Manifest,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Answer {
    /// `resumed` is set when the receiver continues an earlier, interrupted transfer.
    Accept { device_name: String, resumed: Option<Resumed> },
    Decline,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resumed {
    pub files_done: u32,
    pub bytes_left: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FileRequest {
    /// Index into the manifest's file list.
    pub index: u32,
    /// First chunk wanted (1 KiB chunks); everything from there to the end is sent.
    pub from_chunk: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Done {
    pub files: u32,
}

/// Sent by the receiver after [`Done`]: where each offered item was saved, in the order of
/// the manifest's roots, as the receiver describes it ("~/Downloads/glft/Album"), so the
/// sender can say where the files went. A sender from before this message never reads it.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Saved {
    pub places: Vec<String>,
}

pub async fn write<T: Serialize>(stream: &mut (impl AsyncWrite + Unpin), msg: &T) -> Result<(), Error> {
    let bytes = postcard::to_stdvec(msg).map_err(|e| Error::new(ErrorKind::ProtocolError, e.to_string()))?;
    let len = u32::try_from(bytes.len())
        .ok()
        .filter(|l| *l <= MAX_MESSAGE)
        .ok_or_else(|| Error::new(ErrorKind::ProtocolError, "message too large"))?;
    stream.write_all(&len.to_be_bytes()).await.map_err(lost)?;
    stream.write_all(&bytes).await.map_err(lost)?;
    Ok(())
}

pub async fn read<T: DeserializeOwned>(stream: &mut (impl tokio::io::AsyncRead + Unpin)) -> Result<T, Error> {
    let len = stream.read_u32().await.map_err(lost)?;
    if len > MAX_MESSAGE {
        return Err(Error::new(ErrorKind::ProtocolError, format!("message of {len} bytes")));
    }
    let mut buf = vec![0; len as usize];
    stream.read_exact(&mut buf).await.map_err(lost)?;
    postcard::from_bytes(&buf).map_err(|e| Error::new(ErrorKind::ProtocolError, e.to_string()))
}

pub(crate) fn lost(e: impl std::fmt::Display) -> Error {
    Error::new(ErrorKind::ConnectionLost, e.to_string())
}

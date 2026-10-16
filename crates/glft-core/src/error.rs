//! Errors as sentences.
//!
//! Every error the user can see has a kind with a short id, a headline saying what
//! happened and why, and a next step. The raw system error is kept as `detail` and
//! never goes in the headline (design rule 5). This is the seed of the error catalogue.

use std::{fmt, io, path::Path};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    NoHomeDir,
    DataDirUnwritable,
    KeyFileDamaged,
    BadDeviceId,
    BindFailed,
    PortInUse,
    PeerUnreachable,
    WrongProgram,
    ConnectionLost,
    Declined,
    ProtocolError,
    NothingToSend,
    SourceUnreadable,
    NameNotText,
    DuplicateName,
    UnsafePath,
    ReceiveDirUnwritable,
    DiskFull,
    VerificationFailed,
}

impl ErrorKind {
    /// Short stable id, used for help pages and in exported logs.
    pub fn id(self) -> &'static str {
        match self {
            Self::NoHomeDir => "no-home-dir",
            Self::DataDirUnwritable => "data-dir-unwritable",
            Self::KeyFileDamaged => "key-file-damaged",
            Self::BadDeviceId => "bad-device-id",
            Self::BindFailed => "bind-failed",
            Self::PortInUse => "port-in-use",
            Self::PeerUnreachable => "peer-unreachable",
            Self::WrongProgram => "wrong-program",
            Self::ConnectionLost => "connection-lost",
            Self::Declined => "declined",
            Self::ProtocolError => "protocol-error",
            Self::NothingToSend => "nothing-to-send",
            Self::SourceUnreadable => "source-unreadable",
            Self::NameNotText => "name-not-text",
            Self::DuplicateName => "duplicate-name",
            Self::UnsafePath => "unsafe-path",
            Self::ReceiveDirUnwritable => "receive-dir-unwritable",
            Self::DiskFull => "disk-full",
            Self::VerificationFailed => "verification-failed",
        }
    }

    /// What happened, and why, in one sentence.
    pub fn headline(self) -> &'static str {
        match self {
            Self::NoHomeDir => "glft could not find your home folder, so it has nowhere to keep its files.",
            Self::DataDirUnwritable => "glft could not write to its own data folder.",
            Self::KeyFileDamaged => "This device's key file is damaged, so glft cannot prove who it is.",
            Self::BadDeviceId => "That device id is not valid; it should be 64 letters and digits.",
            Self::BindFailed => "glft could not open a network port on this computer.",
            Self::PortInUse => "The port you chose is already in use by another program.",
            Self::PeerUnreachable => "The other device did not answer.",
            Self::WrongProgram => "Something answered, but it is not a compatible version of glft.",
            Self::ConnectionLost => "The connection to the other device was lost.",
            Self::Declined => "The other device declined the files.",
            Self::ProtocolError => "The other device sent something glft did not expect.",
            Self::NothingToSend => "There is nothing to send.",
            Self::SourceUnreadable => "A file you chose to send could not be read.",
            Self::NameNotText => "A file name is not valid text, so the other device could not store it.",
            Self::DuplicateName => "Two of the chosen items have the same name.",
            Self::UnsafePath => "The other device offered a file path that points outside the receive folder, so the offer was refused.",
            Self::ReceiveDirUnwritable => "glft could not write to the receive folder.",
            Self::DiskFull => "The disk is full.",
            Self::VerificationFailed => "A file did not match its fingerprint, so it was not saved.",
        }
    }

    /// What the user can do about it.
    pub fn next_step(self) -> &'static str {
        match self {
            Self::NoHomeDir => "Set HOME, or set GLFT_DATA_DIR to a folder glft can use.",
            Self::DataDirUnwritable => "Check that ~/.local/share/glft is yours and the disk is not full.",
            Self::KeyFileDamaged => "Move ~/.local/share/glft/secret.key aside and start glft again; devices will see this computer as new.",
            Self::BadDeviceId => "Copy the id again from `glft receive` on the other device.",
            Self::BindFailed => "Check that no firewall or sandbox stops glft from using the network.",
            Self::PortInUse => "Choose another port with --port, or leave --port out and glft picks one.",
            Self::PeerUnreachable => "Check that `glft receive` is running there and both devices are on the same network, or give its address with --addr.",
            Self::WrongProgram => "Update glft on both devices to the same version.",
            Self::ConnectionLost => "Check the network and send again.",
            Self::Declined => "Ask the person on the other device to accept, then send again.",
            Self::ProtocolError => "Update glft on both devices to the same version.",
            Self::NothingToSend => "Choose at least one file or folder.",
            Self::SourceUnreadable => "Check that the file still exists and that you can open it.",
            Self::NameNotText => "Rename the file so its name is plain text, then send again.",
            Self::DuplicateName => "Send them separately, or rename one of them.",
            Self::UnsafePath => "Nothing was saved. If you trust the sender, ask them to update glft.",
            Self::ReceiveDirUnwritable => "Choose another receive folder with --dir, or check its permissions.",
            Self::DiskFull => "Free some space on this device, then send again.",
            Self::VerificationFailed => "Send again; if it keeps failing, the file may be changing while it is sent.",
        }
    }
}

/// An error the user can read: kind plus the raw detail.
#[derive(Debug)]
pub struct Error {
    pub kind: ErrorKind,
    /// The raw system or library error. Shown under the headline, never in it.
    pub detail: String,
}

impl Error {
    pub fn new(kind: ErrorKind, detail: impl Into<String>) -> Self {
        Self { kind, detail: detail.into() }
    }

    /// An I/O error about `path`; a full disk is reported as such whatever `kind` says.
    pub fn io(kind: ErrorKind, path: &Path, err: io::Error) -> Self {
        let kind = if err.kind() == io::ErrorKind::StorageFull { ErrorKind::DiskFull } else { kind };
        Self::new(kind, format!("{}: {err}", path.display()))
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.kind.headline())
    }
}

impl std::error::Error for Error {}

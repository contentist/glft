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
    NetworkDenied,
    PortInUse,
    PeerUnreachable,
    NotPaired,
    LocalNetworkBlocked,
    /// A VPN with "Block connections without VPN" (lockdown) cuts the phone off from the
    /// local network, whatever the VPN app's own LAN setting says.
    VpnLockdown,
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
    DevicesFileDamaged,
    UnknownDevice,
    AmbiguousDevice,
    PairingCodeUnreadable,
    PairingCodeWrong,
    PairingNoAnswer,
    PairingExpired,
    PairingAbandoned,
    EngineNotRunning,
    EngineNotAnswering,
    Cancelled,
    EngineAlreadyRunning,
    NoSuchOffer,
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
            Self::NetworkDenied => "network-denied",
            Self::PortInUse => "port-in-use",
            Self::PeerUnreachable => "peer-unreachable",
            Self::NotPaired => "not-paired",
            Self::LocalNetworkBlocked => "local-network-blocked",
            Self::VpnLockdown => "vpn-lockdown",
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
            Self::DevicesFileDamaged => "devices-file-damaged",
            Self::UnknownDevice => "unknown-device",
            Self::AmbiguousDevice => "ambiguous-device",
            Self::PairingCodeUnreadable => "pairing-code-unreadable",
            Self::PairingCodeWrong => "pairing-code-wrong",
            Self::PairingNoAnswer => "pairing-no-answer",
            Self::PairingExpired => "pairing-expired",
            Self::PairingAbandoned => "pairing-abandoned",
            Self::EngineNotRunning => "engine-not-running",
            Self::EngineNotAnswering => "engine-not-answering",
            Self::Cancelled => "cancelled",
            Self::EngineAlreadyRunning => "engine-already-running",
            Self::NoSuchOffer => "no-such-offer",
        }
    }

    /// What happened, and why, in one sentence.
    pub fn headline(self) -> &'static str {
        match self {
            Self::NoHomeDir => "glft could not find your home folder, so it has nowhere to keep its files.",
            Self::DataDirUnwritable => "glft could not write to its own data folder.",
            Self::KeyFileDamaged => "This device's key file is damaged, so glft cannot prove who it is.",
            Self::BadDeviceId => "That device id is not valid; it should be 64 letters and digits.",
            Self::BindFailed => "glft could not open a network port on this device.",
            Self::NetworkDenied => "This device does not let glft use the network at all.",
            Self::PortInUse => "The port you chose is already in use by another program.",
            Self::PeerUnreachable => "The other device did not answer.",
            Self::NotPaired => "The other device refused the connection because it is not paired with this one.",
            Self::LocalNetworkBlocked => "Local network access is off for glft, so it cannot reach devices on your network.",
            Self::VpnLockdown => "A VPN on this phone is blocking connections to your local network.",
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
            Self::DevicesFileDamaged => "The list of paired devices is damaged, so glft cannot tell which devices to trust.",
            Self::UnknownDevice => "No paired device has that name or id.",
            Self::AmbiguousDevice => "More than one paired device matches that name.",
            Self::PairingCodeUnreadable => "That is not a glft pairing code, or part of it is missing.",
            Self::PairingCodeWrong => "The computer did not accept the code; it may have shown a newer one since.",
            Self::PairingNoAnswer => "The device that showed the code did not answer.",
            Self::PairingExpired => "Nobody scanned the code within 5 minutes, so it no longer works.",
            Self::PairingAbandoned => "The other device stopped before pairing finished.",
            Self::EngineNotRunning => "glft's background engine is not running and could not be started.",
            Self::EngineNotAnswering => "glft's background engine is running but not answering.",
            Self::Cancelled => "Stopped on request.",
            Self::EngineAlreadyRunning => "glft's engine is already running.",
            Self::NoSuchOffer => "There is no offer with that number waiting for an answer.",
        }
    }

    /// What the user can do about it.
    pub fn next_step(self) -> &'static str {
        match self {
            Self::NoHomeDir => "Set HOME, or set GLFT_DATA_DIR to a folder glft can use.",
            Self::DataDirUnwritable => "Check that ~/.local/share/glft is yours and the disk is not full.",
            Self::KeyFileDamaged => "Move ~/.local/share/glft/secret.key aside and start glft again; other devices will see this one as new.",
            Self::BadDeviceId => "Use the device's name as `glft devices` lists it, or copy its full id again.",
            Self::BindFailed => "Check that no firewall or sandbox stops glft from using the network.",
            Self::NetworkDenied => "On GrapheneOS, turn on glft's Network permission in Settings, under Apps, glft, Permissions. On a computer, check that no sandbox blocks it.",
            Self::PortInUse => "Choose another port with --port, or leave --port out and glft picks one.",
            Self::PeerUnreachable => "Check that receiving is switched on there (on a computer: `glft receive on`) and that both devices are on the same network. In the terminal you can also give its address with --addr.",
            Self::NotPaired => "Pair the two devices first (`glft pair` on the computer, then scan its code), then send again.",
            Self::LocalNetworkBlocked => "Allow glft's local network permission in Settings, under Apps, glft, Permissions.",
            Self::VpnLockdown => "While you transfer, switch off \"Block connections without VPN\" in Settings, Network & internet, VPN, the gear next to your VPN. If your VPN app has a local network (LAN) option, turn it on as well. Or, if both devices are on the same mesh such as Yggdrasil, glft can send over it: pair them again while both are on it.",
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
            Self::DevicesFileDamaged => "Move ~/.local/share/glft/devices.json aside and pair your devices again.",
            Self::UnknownDevice => "Run `glft devices` to see the paired devices, or pair it first with `glft pair`.",
            Self::AmbiguousDevice => "Use more of the device's id, as `glft devices` lists it.",
            Self::PairingCodeUnreadable => "Scan the code again, or copy the whole line under it.",
            Self::PairingCodeWrong => "Scan the code the computer shows now.",
            Self::PairingNoAnswer => "Check that both devices are on the same network and the code is still showing; codes stop working after 5 minutes or once used, so run `glft pair` again if needed.",
            Self::PairingExpired => "Run `glft pair` again and scan the new code.",
            Self::PairingAbandoned => "Start pairing again on both devices.",
            Self::EngineNotRunning => "Run `glft setup`, or start it by hand with `glftd` to see why it stops.",
            Self::EngineNotAnswering => "Run `glft restart` to restart it.",
            Self::Cancelled => "Nothing to do.",
            Self::EngineAlreadyRunning => "Use `glft status` to see it, or `glft restart` to restart it.",
            Self::NoSuchOffer => "Run `glft offers` to see the offers waiting; it may have been answered or withdrawn already.",
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

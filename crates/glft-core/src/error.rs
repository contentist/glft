//! Errors as sentences.
//!
//! Every error the user can see has a kind with a short id, a headline saying what
//! happened and why, and a next step. The raw system error is kept as `detail` and
//! never goes in the headline (design rule 5). This is the error catalogue: each id also
//! has a help page, `docs/help/<id>.md`.

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
    /// The window was given something that has no path on this computer (MTP, a network
    /// location without FUSE).
    NotALocalFile,
    SendAlreadyRunning,
    /// Android did not start, or ended, the phone's send job before it began.
    SendNotStarted,
    TextNotSupported,
    /// Android's daily limit on background receiving was reached.
    ReceiveTimeLimit,
    /// The receiving device's disk filled up, as it told this one.
    PeerDiskFull,
    /// The receiving device can't write its receive folder, as it told this one.
    PeerCantSave,
}

/// Where an error is shown, which decides how its next step is worded: the terminal can
/// be told about `glft pair`, the phone about its Settings app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    Terminal,
    Window,
    Phone,
}

static SURFACE: std::sync::OnceLock<Surface> = std::sync::OnceLock::new();

/// Sets where this program shows errors; the terminal unless set. First call wins.
pub fn set_surface(surface: Surface) {
    let _ = SURFACE.set(surface);
}

pub fn surface() -> Surface {
    SURFACE.get().copied().unwrap_or(Surface::Terminal)
}

impl ErrorKind {
    /// Every kind, in declaration order (checked by a test against [`Self::index`]).
    pub const ALL: &[ErrorKind] = &[
        Self::NoHomeDir,
        Self::DataDirUnwritable,
        Self::KeyFileDamaged,
        Self::BadDeviceId,
        Self::BindFailed,
        Self::NetworkDenied,
        Self::PortInUse,
        Self::PeerUnreachable,
        Self::NotPaired,
        Self::LocalNetworkBlocked,
        Self::VpnLockdown,
        Self::WrongProgram,
        Self::ConnectionLost,
        Self::Declined,
        Self::ProtocolError,
        Self::NothingToSend,
        Self::SourceUnreadable,
        Self::NameNotText,
        Self::DuplicateName,
        Self::UnsafePath,
        Self::ReceiveDirUnwritable,
        Self::DiskFull,
        Self::VerificationFailed,
        Self::DevicesFileDamaged,
        Self::UnknownDevice,
        Self::AmbiguousDevice,
        Self::PairingCodeUnreadable,
        Self::PairingCodeWrong,
        Self::PairingNoAnswer,
        Self::PairingExpired,
        Self::PairingAbandoned,
        Self::EngineNotRunning,
        Self::EngineNotAnswering,
        Self::Cancelled,
        Self::EngineAlreadyRunning,
        Self::NoSuchOffer,
        Self::NotALocalFile,
        Self::SendAlreadyRunning,
        Self::SendNotStarted,
        Self::TextNotSupported,
        Self::ReceiveTimeLimit,
        Self::PeerDiskFull,
        Self::PeerCantSave,
    ];

    /// Its place in [`Self::ALL`]. The match is exhaustive, so a new kind can't be
    /// forgotten here, and the test then finds it missing from `ALL`.
    pub fn index(self) -> usize {
        match self {
            Self::NoHomeDir => 0,
            Self::DataDirUnwritable => 1,
            Self::KeyFileDamaged => 2,
            Self::BadDeviceId => 3,
            Self::BindFailed => 4,
            Self::NetworkDenied => 5,
            Self::PortInUse => 6,
            Self::PeerUnreachable => 7,
            Self::NotPaired => 8,
            Self::LocalNetworkBlocked => 9,
            Self::VpnLockdown => 10,
            Self::WrongProgram => 11,
            Self::ConnectionLost => 12,
            Self::Declined => 13,
            Self::ProtocolError => 14,
            Self::NothingToSend => 15,
            Self::SourceUnreadable => 16,
            Self::NameNotText => 17,
            Self::DuplicateName => 18,
            Self::UnsafePath => 19,
            Self::ReceiveDirUnwritable => 20,
            Self::DiskFull => 21,
            Self::VerificationFailed => 22,
            Self::DevicesFileDamaged => 23,
            Self::UnknownDevice => 24,
            Self::AmbiguousDevice => 25,
            Self::PairingCodeUnreadable => 26,
            Self::PairingCodeWrong => 27,
            Self::PairingNoAnswer => 28,
            Self::PairingExpired => 29,
            Self::PairingAbandoned => 30,
            Self::EngineNotRunning => 31,
            Self::EngineNotAnswering => 32,
            Self::Cancelled => 33,
            Self::EngineAlreadyRunning => 34,
            Self::NoSuchOffer => 35,
            Self::NotALocalFile => 36,
            Self::SendAlreadyRunning => 37,
            Self::SendNotStarted => 38,
            Self::TextNotSupported => 39,
            Self::ReceiveTimeLimit => 40,
            Self::PeerDiskFull => 41,
            Self::PeerCantSave => 42,
        }
    }

    /// The kind with this id, e.g. from an [`Error`] that crossed the engine's socket.
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|k| k.id() == id)
    }

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
            Self::NotALocalFile => "not-a-local-file",
            Self::SendAlreadyRunning => "send-already-running",
            Self::SendNotStarted => "send-not-started",
            Self::TextNotSupported => "text-not-supported",
            Self::ReceiveTimeLimit => "receive-time-limit",
            Self::PeerDiskFull => "peer-disk-full",
            Self::PeerCantSave => "peer-cant-save",
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
            Self::NotALocalFile => "Some of these aren't files on this computer, so glft can't send them.",
            Self::SendAlreadyRunning => "A send is already running.",
            Self::SendNotStarted => "The send didn't start.",
            Self::TextNotSupported => "glft sends files; sharing text or links isn't possible yet.",
            Self::ReceiveTimeLimit => "Receiving stopped: Android allows background receiving for 6 hours a day.",
            Self::PeerDiskFull => "The other device's disk is full, so the rest can't be saved there.",
            Self::PeerCantSave => "The other device can't write to its receive folder.",
        }
    }

    /// What the user can do about it, worded for where this program shows it.
    pub fn next_step(self) -> &'static str {
        self.next_step_on(surface())
    }

    /// What the user can do about it, worded for `surface`.
    pub fn next_step_on(self, surface: Surface) -> &'static str {
        use Surface::*;
        match (self, surface) {
            (Self::NoHomeDir, Terminal) => "Set HOME, or set GLFT_DATA_DIR to a folder glft can use.",
            (Self::NoHomeDir, _) => "Log out and in again; if it keeps happening, ask for help and include the details below.",
            (Self::DataDirUnwritable, Phone) => "Free some space on the phone, then open glft again.",
            (Self::DataDirUnwritable, _) => "Check that ~/.local/share/glft is yours and the disk is not full.",
            (Self::KeyFileDamaged, Phone) => "In Settings, Apps, glft, Storage, clear glft's storage, then pair again; other devices will see this phone as new.",
            (Self::KeyFileDamaged, _) => "Move ~/.local/share/glft/secret.key aside and start glft again; other devices will see this one as new.",
            (Self::BadDeviceId, Terminal) => "Use the device's name as `glft devices` lists it, or copy its full id again.",
            (Self::BadDeviceId, _) => "Pair the device again.",
            (Self::BindFailed, Phone) => "Check glft's Network permission in Settings, Apps, glft, Permissions, then try again.",
            (Self::BindFailed, _) => "Check that no firewall or sandbox stops glft from using the network.",
            (Self::NetworkDenied, Phone) => "Turn on glft's Network permission in Settings, Apps, glft, Permissions.",
            (Self::NetworkDenied, _) => "Check that no firewall or sandbox stops glft from using the network.",
            (Self::PortInUse, Terminal) => "Choose another port with --port, or leave --port out and glft picks one.",
            (Self::PortInUse, _) => "Switch receiving off and on again; glft then picks a free port.",
            (Self::PeerUnreachable, Terminal) => "Check that receiving is on there (the switch in glft on the phone, or `glft receive on` on a computer) and that both devices are on the same network. You can also give its address with --addr.",
            (Self::PeerUnreachable, Window) => "Check that receiving is switched on in glft on the phone, and that the phone is on the same Wi-Fi as this computer.",
            (Self::PeerUnreachable, Phone) => "Check that receiving is switched on in glft on the computer (the switch at the top of its window, or `glft receive on`), and that the phone is on the same Wi-Fi.",
            (Self::NotPaired, Terminal) => "Pair the two devices first (`glft pair` here, then scan its code with the phone), then send again.",
            (Self::NotPaired, Window) => "Pair the two devices first (\"Pair a phone…\" in this window, then scan the code with the phone), then send again.",
            (Self::NotPaired, Phone) => "Pair again: on the computer choose \"Pair a phone…\" in glft (or run `glft pair`), then tap \"Pair with a computer\" here and scan the code.",
            (Self::LocalNetworkBlocked, Phone) => "Allow glft's local network permission in Settings, under Apps, glft, Permissions.",
            (Self::LocalNetworkBlocked, _) => "Check that no firewall blocks glft on the local network.",
            (Self::VpnLockdown, _) => "While you transfer, switch off \"Block connections without VPN\" in Settings, Network & internet, VPN, the gear next to your VPN. If your VPN app has a local network (LAN) option, turn it on as well. Or, if both devices are on the same mesh such as Yggdrasil, glft can send over it: pair them again while both are on it.",
            (Self::WrongProgram, _) => "Update glft on both devices to the same version.",
            (Self::ConnectionLost, _) => "Check the network and send again; an interrupted transfer continues where it stopped.",
            (Self::Declined, _) => "Ask the person on the other device to accept, then send again.",
            (Self::ProtocolError, _) => "Update glft on both devices to the same version.",
            (Self::NothingToSend, _) => "Choose at least one file or folder.",
            (Self::SourceUnreadable, _) => "Check that the file still exists and that you can open it, then send again.",
            (Self::NameNotText, _) => "Rename the file so its name is plain text, then send again.",
            (Self::DuplicateName, _) => "Send them separately, or rename one of them.",
            (Self::UnsafePath, _) => "Nothing was saved. If you trust the sender, ask them to update glft.",
            (Self::ReceiveDirUnwritable, Terminal) => "Choose another receive folder with --dir, or check its permissions.",
            (Self::ReceiveDirUnwritable, Window) => "Check that the receive folder (~/Downloads/glft) is yours and the disk isn't full.",
            (Self::ReceiveDirUnwritable, Phone) => "Check that the phone has space; if you chose a folder to save into, choose it again in glft.",
            (Self::DiskFull, _) => "Free some space on this device, then send again; what arrived already is kept.",
            (Self::VerificationFailed, _) => "Send again; if it keeps failing, the file may be changing while it is sent.",
            (Self::DevicesFileDamaged, Phone) => "In Settings, Apps, glft, Storage, clear glft's storage, then pair your devices again.",
            (Self::DevicesFileDamaged, _) => "Move ~/.local/share/glft/devices.json aside and pair your devices again.",
            (Self::UnknownDevice, Terminal) => "Run `glft devices` to see the paired devices, or pair it first with `glft pair`.",
            (Self::UnknownDevice, _) => "Pair it first, then send again.",
            (Self::AmbiguousDevice, Terminal) => "Use more of the device's id, as `glft devices` lists it.",
            (Self::AmbiguousDevice, _) => "Forget the old one of the two, or rename one of the devices, then send again.",
            (Self::PairingCodeUnreadable, Terminal) => "Copy the whole code again, or scan it with the phone instead.",
            (Self::PairingCodeUnreadable, _) => "Scan the code again, or copy the whole line under it.",
            (Self::PairingCodeWrong, _) => "Scan the code the computer shows now.",
            (Self::PairingNoAnswer, Terminal) => "Check that both devices are on the same network and the code is still showing; codes stop working after 5 minutes or once used, so run `glft pair` again if needed.",
            (Self::PairingNoAnswer, Window) => "Check that the phone is on the same Wi-Fi as this computer and that the code is still showing; codes stop working after 5 minutes or once used, so start pairing again if needed.",
            (Self::PairingNoAnswer, Phone) => "Check that the phone is on the same Wi-Fi as the computer and that the code is still showing; if it ran out, start pairing again on the computer.",
            (Self::PairingExpired, Terminal) => "Run `glft pair` again and scan the new code.",
            (Self::PairingExpired, Window) => "Click \"Pair a phone…\" again and scan the new code.",
            (Self::PairingExpired, Phone) => "Start pairing again on the computer and scan the new code.",
            (Self::PairingAbandoned, _) => "Start pairing again on both devices.",
            (Self::EngineNotRunning, Terminal) => "Run `glft setup`, or start it by hand with `glftd` to see why it stops.",
            (Self::EngineNotRunning, _) => "In a terminal, run `glft setup`, or start `glftd` by hand to see why it stops.",
            (Self::EngineNotAnswering, Terminal) => "Run `glft restart` to restart it.",
            (Self::EngineNotAnswering, _) => "In a terminal, run `glft restart`; this window reconnects by itself.",
            (Self::Cancelled, _) => "Nothing to do.",
            (Self::EngineAlreadyRunning, _) => "Use `glft status` to see it, or `glft restart` to restart it.",
            (Self::NoSuchOffer, Terminal) => "Run `glft offers` to see the offers waiting; it may have been answered or withdrawn already.",
            (Self::NoSuchOffer, _) => "It was probably answered or withdrawn already; nothing to do.",
            (Self::NotALocalFile, _) => "Copy them to a folder on this computer first, then send them from there.",
            (Self::SendAlreadyRunning, _) => "Wait for it to finish, or cancel it, then send again.",
            (Self::SendNotStarted, Phone) => "Send the files again; if it keeps happening, restart the phone.",
            (Self::SendNotStarted, _) => "Send the files again.",
            (Self::TextNotSupported, _) => "Share a file instead, or save the text to a file first.",
            (Self::ReceiveTimeLimit, _) => "Open glft and switch receiving on again.",
            (Self::PeerDiskFull, _) => "Free some space on the other device, then send again; what arrived there is kept, and the transfer continues from there.",
            (Self::PeerCantSave, _) => "On the other device, check its receive folder (glft's log there says which and why), then send again.",
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_is_listed_once_with_a_unique_id() {
        for (i, k) in ErrorKind::ALL.iter().enumerate() {
            assert_eq!(k.index(), i, "{k:?} is out of place in ErrorKind::ALL");
            assert_eq!(ErrorKind::from_id(k.id()), Some(*k));
        }
        let mut ids: Vec<_> = ErrorKind::ALL.iter().map(|k| k.id()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), ErrorKind::ALL.len(), "two kinds share an id");
    }

    #[test]
    fn every_kind_has_a_sentence_and_a_next_step_everywhere() {
        for k in ErrorKind::ALL {
            let h = k.headline();
            assert!(h.ends_with('.') && h.len() > 15, "{k:?}: headline {h:?}");
            // Raw codes belong in the details (design rule 5).
            assert!(!h.contains("os error") && !h.contains("errno"), "{k:?}: code in headline");
            for s in [Surface::Terminal, Surface::Window, Surface::Phone] {
                let n = k.next_step_on(s);
                assert!(n.ends_with('.') && n.len() > 10, "{k:?} on {s:?}: next step {n:?}");
                // Terminal flags mean nothing in a window or on the phone.
                if s != Surface::Terminal {
                    assert!(!n.contains("--"), "{k:?} on {s:?}: terminal flag in {n:?}");
                }
            }
        }
    }
}

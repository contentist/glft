//! Help pages, one per error id (`docs/help/<id>.md`), built into every program so help
//! works offline and matches the installed version.

/// Topics that aren't errors, listed by `glft help` after the error pages.
const EXTRA: &[(&str, &str)] = &[("wifi-freezes", include_str!("../../../docs/help/wifi-freezes.md"))];

macro_rules! pages {
    ($($id:literal),* $(,)?) => {
        &[$(($id, include_str!(concat!("../../../docs/help/", $id, ".md")))),*]
    };
}

/// Every error's page, by id. A missing file stops the build.
const ERRORS: &[(&str, &str)] = pages![
    "no-home-dir",
    "data-dir-unwritable",
    "key-file-damaged",
    "bad-device-id",
    "bind-failed",
    "network-denied",
    "port-in-use",
    "peer-unreachable",
    "not-paired",
    "local-network-blocked",
    "vpn-lockdown",
    "wrong-program",
    "connection-lost",
    "declined",
    "protocol-error",
    "nothing-to-send",
    "source-unreadable",
    "name-not-text",
    "duplicate-name",
    "unsafe-path",
    "receive-dir-unwritable",
    "disk-full",
    "verification-failed",
    "devices-file-damaged",
    "unknown-device",
    "ambiguous-device",
    "pairing-code-unreadable",
    "pairing-code-wrong",
    "pairing-no-answer",
    "pairing-expired",
    "pairing-abandoned",
    "engine-not-running",
    "engine-not-answering",
    "cancelled",
    "engine-already-running",
    "no-such-offer",
    "not-a-local-file",
    "send-already-running",
    "send-not-started",
    "text-not-supported",
    "receive-time-limit",
    "peer-disk-full",
    "peer-cant-save",
];

/// The page for `id` (an error id, or another topic), as Markdown.
pub fn page(id: &str) -> Option<&'static str> {
    ERRORS.iter().chain(EXTRA).find(|(i, _)| *i == id).map(|(_, p)| *p)
}

/// Every topic: its id and title (the page's first line, without `# `).
pub fn topics() -> Vec<(&'static str, &'static str)> {
    ERRORS.iter().chain(EXTRA).map(|(id, p)| (*id, p.lines().next().unwrap_or_default().trim_start_matches("# "))).collect()
}

#[cfg(test)]
mod tests {
    use crate::ErrorKind;

    #[test]
    fn every_error_has_a_help_page_with_the_three_parts() {
        for k in ErrorKind::ALL {
            let page = super::page(k.id()).unwrap_or_else(|| panic!("no help page for {}", k.id()));
            assert!(page.starts_with("# "), "{}: no title", k.id());
            for part in ["## What happened", "## Why", "## What to do"] {
                assert!(page.contains(part), "{}: no \"{part}\"", k.id());
            }
        }
        assert_eq!(super::ERRORS.len(), ErrorKind::ALL.len(), "a help page for an error that doesn't exist");
    }

    #[test]
    fn pages_name_only_topics_that_exist() {
        // "see **vpn-lockdown**" must lead somewhere.
        for (id, page) in super::ERRORS.iter().chain(super::EXTRA) {
            for word in page.split("**").skip(1).step_by(2) {
                if word.contains('-') && word.chars().all(|c| c.is_ascii_lowercase() || c == '-') {
                    assert!(super::page(word).is_some(), "{id} refers to a missing page {word}");
                }
            }
        }
    }
}

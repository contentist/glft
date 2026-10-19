//! `glft setup` from an unpacked download, then `glft uninstall`, in a private home folder
//! (no systemd, no desktop): files appear, the engine runs from the installed copy, and
//! uninstall stops it before removing anything.

use std::{
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

struct Home {
    dir: tempfile::TempDir,
}

impl Home {
    fn new() -> Self {
        Self { dir: tempfile::tempdir().unwrap() }
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    /// Runs `program ARGS` as a user whose home is this folder; returns its output.
    fn run(&self, program: &Path, args: &[&str]) -> String {
        let out = Command::new(program)
            .args(args)
            .env_clear()
            .env("HOME", self.dir.path())
            .env("PATH", "/usr/bin:/bin")
            .env("GLFT_NO_SYSTEMD", "1")
            .env("GLFT_NO_DESKTOP", "1")
            .env("GLFT_RUNTIME_DIR", self.path("run"))
            .output()
            .unwrap();
        let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        assert!(out.status.success(), "{} {args:?} failed:\n{text}", program.display());
        text
    }
}

/// The programs as a download unpacks them: side by side in one folder.
fn unpacked(home: &Home) -> PathBuf {
    let dir = home.path("glft-0.1.0");
    std::fs::create_dir_all(&dir).unwrap();
    let cli = PathBuf::from(env!("CARGO_BIN_EXE_glft"));
    let built = cli.parent().unwrap();
    for name in ["glft", "glftd", "glft-window"] {
        let src = built.join(name);
        if name == "glftd" {
            assert!(src.exists(), "build glftd first (`cargo build`)");
        }
        if src.exists() {
            std::fs::copy(&src, dir.join(name)).unwrap();
        }
    }
    dir
}

fn running(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
        && !std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|s| s.split_whitespace().nth(2) == Some("Z"))
}

#[test]
fn setup_installs_a_download_and_uninstall_stops_the_engine_then_removes_it() {
    let home = Home::new();
    let download = unpacked(&home);
    let out = home.run(&download.join("glft"), &["setup"]);
    assert!(out.contains("Installed glft in ~/.local/lib/glft"), "{out}");
    for name in ["glft", "glftd"] {
        assert!(home.path(&format!(".local/lib/glft/{name}")).exists());
        assert_eq!(std::fs::read_link(home.path(&format!(".local/bin/{name}"))).unwrap(), home.path(&format!(".local/lib/glft/{name}")));
    }
    if download.join("glft-window").exists() {
        let entry = std::fs::read_to_string(home.path(".local/share/applications/io.github.contentist.glft.desktop")).unwrap();
        assert!(entry.contains(&format!("Exec={} %F", home.path(".local/lib/glft/glft-window").display())));
        assert!(home.path(".local/share/nemo/actions/glft-send.nemo_action").exists());
    }

    // The engine now starts from the installed copy, not the download.
    let status = home.run(&home.path(".local/bin/glft"), &["status"]);
    let pid: u32 = status.split("pid ").nth(1).and_then(|s| s.split(')').next()).and_then(|p| p.parse().ok()).expect(&status);
    assert_eq!(std::fs::read_link(format!("/proc/{pid}/exe")).unwrap(), home.path(".local/lib/glft/glftd"));

    let out = home.run(&home.path(".local/bin/glft"), &["uninstall"]);
    assert!(out.contains("Stopped the running engine first"), "{out}");
    let deadline = Instant::now() + Duration::from_secs(3);
    while running(pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!running(pid), "the engine still runs after uninstall");
    assert!(!home.path(".local/lib/glft").exists());
    assert!(!home.path(".local/bin/glft").exists());
    assert!(!home.path(".local/share/applications/io.github.contentist.glft.desktop").exists());
    // The key and pairings stay unless asked.
    assert!(home.path(".local/share/glft").exists(), "{out}");
    assert!(out.contains("Kept this computer's key and pairings"), "{out}");

    let out = home.run(&download.join("glft"), &["uninstall", "--purge"]);
    assert!(!home.path(".local/share/glft").exists(), "{out}");
}

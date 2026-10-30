//! The five platforms a plugin binary can be staged for, and the file layout
//! each one uses under `plugins/<plugin>/libexec/`.

use std::fmt;

/// A staged platform. The key is the directory name the generated dispatcher
/// derives from `uname -s` and `uname -m`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Platform {
    /// `linux-x86_64`
    LinuxX86_64,
    /// `linux-aarch64`
    LinuxAarch64,
    /// `macos-x86_64`
    MacosX86_64,
    /// `macos-aarch64`
    MacosAarch64,
    /// `windows-x86_64`
    WindowsX86_64,
}

impl Platform {
    /// Every platform, in manifest order.
    pub const ALL: [Self; 5] = [
        Self::LinuxX86_64,
        Self::LinuxAarch64,
        Self::MacosX86_64,
        Self::MacosAarch64,
        Self::WindowsX86_64,
    ];

    /// The platform key, for example `linux-x86_64`.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::LinuxX86_64 => "linux-x86_64",
            Self::LinuxAarch64 => "linux-aarch64",
            Self::MacosX86_64 => "macos-x86_64",
            Self::MacosAarch64 => "macos-aarch64",
            Self::WindowsX86_64 => "windows-x86_64",
        }
    }

    /// Parses a platform key.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|platform| platform.key() == key)
    }

    /// The operating system half of the key (`linux`, `macos` or `windows`).
    #[must_use]
    pub fn os(self) -> &'static str {
        match self {
            Self::LinuxX86_64 | Self::LinuxAarch64 => "linux",
            Self::MacosX86_64 | Self::MacosAarch64 => "macos",
            Self::WindowsX86_64 => "windows",
        }
    }

    /// The architecture half of the key (`x86_64` or `aarch64`).
    #[must_use]
    pub fn arch(self) -> &'static str {
        match self {
            Self::LinuxX86_64 | Self::MacosX86_64 | Self::WindowsX86_64 => "x86_64",
            Self::LinuxAarch64 | Self::MacosAarch64 => "aarch64",
        }
    }

    /// Whether binaries for this platform carry the `.exe` suffix.
    #[must_use]
    pub fn is_windows(self) -> bool {
        self == Self::WindowsX86_64
    }

    /// Whether a Rust target triple belongs to this platform (same OS and
    /// architecture).
    #[must_use]
    pub fn matches_triple(self, triple: &str) -> bool {
        let arch_ok = triple.starts_with(&format!("{}-", self.arch()));
        let os_ok = match self.os() {
            "linux" => triple.contains("-linux-"),
            "macos" => triple.ends_with("-apple-darwin"),
            _ => triple.contains("-windows-"),
        };
        arch_ok && os_ok
    }

    /// The staged path of binary `name`, relative to the plugin `libexec/`
    /// directory: `<key>/<name>` on Unix platforms and `<name>.exe` on
    /// Windows. The Windows binary sits beside the dispatcher so that a
    /// native Windows process launch of `libexec/<name>` (which appends `.exe`)
    /// and the dispatcher under Git Bash both reach it.
    #[must_use]
    pub fn staged_path(self, name: &str) -> String {
        if self.is_windows() {
            format!("{name}.exe")
        } else {
            format!("{}/{name}", self.key())
        }
    }
}

impl fmt::Display for Platform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.key())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_round_trip_and_unknown_keys_are_rejected() {
        for platform in Platform::ALL {
            assert_eq!(Platform::from_key(platform.key()), Some(platform));
        }
        assert_eq!(Platform::from_key("linux-armv7"), None);
        assert_eq!(Platform::from_key("Linux-x86_64"), None);
    }

    #[test]
    fn triples_match_only_their_platform() {
        assert!(Platform::LinuxX86_64.matches_triple("x86_64-unknown-linux-gnu"));
        assert!(Platform::LinuxX86_64.matches_triple("x86_64-unknown-linux-musl"));
        assert!(!Platform::LinuxX86_64.matches_triple("aarch64-unknown-linux-gnu"));
        assert!(Platform::MacosAarch64.matches_triple("aarch64-apple-darwin"));
        assert!(!Platform::MacosAarch64.matches_triple("x86_64-apple-darwin"));
        assert!(Platform::WindowsX86_64.matches_triple("x86_64-pc-windows-gnu"));
        assert!(Platform::WindowsX86_64.matches_triple("x86_64-pc-windows-msvc"));
        assert!(!Platform::WindowsX86_64.matches_triple("x86_64-unknown-linux-gnu"));
    }

    #[test]
    fn windows_binaries_sit_beside_the_dispatcher() {
        assert_eq!(Platform::WindowsX86_64.staged_path("tool"), "tool.exe");
        assert_eq!(
            Platform::MacosAarch64.staged_path("tool"),
            "macos-aarch64/tool"
        );
    }
}

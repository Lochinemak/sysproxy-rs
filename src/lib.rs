//! Get/Set system proxy. Supports Windows, macOS and linux (via gsettings).

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

// #[cfg(feature = "utils")]
pub mod utils;

#[cfg(feature = "guard")]
pub mod guard;

#[cfg(feature = "guard")]
pub use guard::{GuardMonitor, GuardType};

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Sysproxy {
    pub host: String,
    pub bypass: String,
    pub port: u16,
    pub enable: bool,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Autoproxy {
    pub url: String,
    pub enable: bool,
}

/// Unflattened system proxy state, one entry per protocol.
///
/// Use this for exact comparisons; [`Sysproxy::get_system_proxy`] returns one merged endpoint.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ProxySnapshot {
    /// SOCKS endpoint and whether it is on.
    pub socks: ProxyEndpoint,
    /// HTTP endpoint and whether it is on.
    pub http: ProxyEndpoint,
    /// HTTPS endpoint and whether it is on.
    pub https: ProxyEndpoint,
    /// PAC URL and whether it is on.
    pub auto: Autoproxy,
    /// Bypass list, shared by all three protocols.
    pub bypass: String,
}

/// One protocol's endpoint state.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ProxyEndpoint {
    pub host: String,
    pub port: u16,
    /// Whether this protocol is on *and* points somewhere usable.
    pub enable: bool,
}

impl ProxySnapshot {
    /// Whether the OS holds no usable proxy.
    #[inline]
    pub fn is_all_disabled(&self) -> bool {
        !self.socks.enable && !self.http.enable && !self.https.enable && !self.auto.enable
    }

    /// Match an enabled target across all protocols, with PAC off.
    ///
    /// Bypass order, duplicates, and surrounding whitespace are ignored.
    #[inline]
    pub fn matches_global(&self, target: &Sysproxy) -> bool {
        if !target.enable {
            return false;
        }

        let points_at = |endpoint: &ProxyEndpoint| {
            endpoint.enable && endpoint.host == target.host && endpoint.port == target.port
        };

        points_at(&self.socks)
            && points_at(&self.http)
            && points_at(&self.https)
            && !self.auto.enable
            && bypass_entries(&self.bypass) == bypass_entries(&target.bypass)
    }

    /// Match an enabled PAC target with every global protocol off.
    #[inline]
    pub fn matches_pac(&self, target: &Autoproxy) -> bool {
        target.enable
            && self.auto.enable
            && self.auto.url == target.url
            && !self.socks.enable
            && !self.http.enable
            && !self.https.enable
    }
}

/// Split a bypass list into comparable entries, ignoring order, duplicates and stray whitespace.
fn bypass_entries(bypass: &str) -> std::collections::BTreeSet<&str> {
    bypass
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .collect()
}

/// How far a multi-step proxy write got before it failed.
///
/// macOS proxy writes span several commands, so failures may leave partial state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriteProgress {
    pub(crate) completed: u8,
    pub(crate) total: u8,
}

impl WriteProgress {
    /// Build progress for exercising consumer recovery paths.
    #[inline]
    pub const fn new(completed: u8, total: u8) -> Self {
        Self { completed, total }
    }

    /// Writes that were accepted by the OS before the failure.
    #[inline]
    pub const fn completed(&self) -> u8 {
        self.completed
    }

    /// Writes the attempted sequence performs in total.
    #[inline]
    pub const fn total(&self) -> u8 {
        self.total
    }

    /// Whether no write was accepted before the failure.
    #[inline]
    pub const fn nothing_written(&self) -> bool {
        self.completed == 0
    }
}

impl std::fmt::Display for WriteProgress {
    #[inline]
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} of {} writes completed", self.completed, self.total)
    }
}

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("failed to parse string `{0}`")]
    ParseStr(String),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error("failed to get default network interface")]
    NetworkInterface,

    #[error("failed to set proxy for this environment")]
    NotSupport,

    #[error("admin privileges required to modify system proxy")]
    RequiresAdminPrivileges,

    /// A failed multi-step write. Inspect the source chain for the underlying [`Error`].
    #[error("proxy write failed ({progress})")]
    ProxyWrite {
        progress: WriteProgress,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },

    #[cfg(target_os = "macos")]
    #[error("failed to interact with SCPreferences")]
    SCPreferences,

    #[cfg(target_os = "macos")]
    #[error("failed to interact with SCDynamicStore")]
    SCDynamicStore,

    #[cfg(target_os = "macos")]
    #[error("networksetup failed: {0}")]
    NetworkSetup(String),

    #[cfg(target_os = "linux")]
    #[error(transparent)]
    Xdg(#[from] xdg::BaseDirectoriesError),

    #[cfg(target_os = "windows")]
    #[error("Windows system call failed: {0}")]
    SystemCall(#[from] windows::Win32Error),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Sysproxy {
    pub const fn is_support() -> bool {
        cfg!(any(
            target_os = "linux",
            target_os = "macos",
            target_os = "windows",
        ))
    }
}

impl Autoproxy {
    pub const fn is_support() -> bool {
        cfg!(any(
            target_os = "linux",
            target_os = "macos",
            target_os = "windows",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::{Autoproxy, ProxyEndpoint, ProxySnapshot, Sysproxy};

    fn endpoint(port: u16, enable: bool) -> ProxyEndpoint {
        ProxyEndpoint {
            host: "127.0.0.1".into(),
            port,
            enable,
        }
    }

    fn target() -> Sysproxy {
        Sysproxy {
            host: "127.0.0.1".into(),
            port: 7890,
            enable: true,
            bypass: "localhost,127.0.0.1,*.local".into(),
        }
    }

    fn all_on() -> ProxySnapshot {
        ProxySnapshot {
            socks: endpoint(7890, true),
            http: endpoint(7890, true),
            https: endpoint(7890, true),
            auto: Autoproxy::default(),
            bypass: "localhost,127.0.0.1,*.local".into(),
        }
    }

    #[test]
    fn a_snapshot_matches_a_target_only_when_every_protocol_agrees() {
        assert!(all_on().matches_global(&target()));

        let mut https_off = all_on();
        https_off.https.enable = false;
        assert!(!https_off.matches_global(&target()));

        let mut wrong_port = all_on();
        wrong_port.http.port = 7891;
        assert!(!wrong_port.matches_global(&target()));
    }

    #[test]
    fn bypass_entries_compare_as_a_set() {
        let mut reordered = all_on();
        reordered.bypass = "*.local, 127.0.0.1 ,localhost".into();
        assert!(reordered.matches_global(&target()));

        let mut extra = all_on();
        extra.bypass = "localhost,127.0.0.1,*.local,example.com".into();
        assert!(!extra.matches_global(&target()));
    }

    #[test]
    fn a_leftover_pac_is_not_a_match() {
        let mut with_pac = all_on();
        with_pac.auto = Autoproxy {
            url: "http://example.com/proxy.pac".into(),
            enable: true,
        };
        assert!(!with_pac.matches_global(&target()));
    }

    #[test]
    fn pac_matches_only_when_no_global_proxy_is_left() {
        let pac = Autoproxy {
            url: "http://127.0.0.1:1/pac".into(),
            enable: true,
        };
        let snapshot = ProxySnapshot {
            auto: pac.clone(),
            ..ProxySnapshot::default()
        };
        assert!(snapshot.matches_pac(&pac));

        let mut with_socks = snapshot.clone();
        with_socks.socks = endpoint(7890, true);
        assert!(!with_socks.matches_pac(&pac));
    }

    #[test]
    fn clean_means_every_protocol_and_pac_is_off() {
        assert!(ProxySnapshot::default().is_all_disabled());
        assert!(!all_on().is_all_disabled());

        let mut only_pac = ProxySnapshot::default();
        only_pac.auto.enable = true;
        assert!(!only_pac.is_all_disabled());
    }

    #[test]
    fn a_disabled_target_is_never_a_match() {
        let mut off = target();
        off.enable = false;
        assert!(!all_on().matches_global(&off));
        assert!(!ProxySnapshot::default().matches_global(&off));

        let pac_off = Autoproxy {
            url: "http://127.0.0.1:1/pac".into(),
            enable: false,
        };
        let snapshot = ProxySnapshot {
            auto: Autoproxy {
                url: pac_off.url.clone(),
                enable: true,
            },
            ..ProxySnapshot::default()
        };
        assert!(!snapshot.matches_pac(&pac_off));
    }

    #[test]
    fn duplicate_and_empty_bypass_entries_are_ignored() {
        let mut noisy = all_on();
        noisy.bypass = "localhost,127.0.0.1,*.local,localhost,,127.0.0.1".into();
        assert!(noisy.matches_global(&target()));
    }

    #[test]
    fn bypass_comparison_is_case_sensitive_and_keeps_special_tokens() {
        let mut cased = all_on();
        cased.bypass = "LOCALHOST,127.0.0.1,*.local".into();
        assert!(!cased.matches_global(&target()));

        let mut with_local = all_on();
        with_local.bypass = "localhost,127.0.0.1,*.local,<local>".into();
        assert!(!with_local.matches_global(&target()));
    }
}

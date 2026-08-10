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

/// How far a multi-step proxy write got before it failed.
///
/// macOS proxy writes span several commands, so failures may leave partial state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriteProgress {
    pub(crate) completed: u8,
    pub(crate) total: u8,
}

impl WriteProgress {
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

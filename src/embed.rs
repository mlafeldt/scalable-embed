//! Embedding API (fork addition, not upstream). Upstream code calls into this
//! module only through the hooks below.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub use crate::auth::{DeviceCodePrompt, LoginProgressSink};

static CONFIG_DIR: OnceLock<PathBuf> = OnceLock::new();
static KEYRING_SERVICE: OnceLock<String> = OnceLock::new();

pub(crate) fn config_dir_override() -> Option<&'static Path> {
    CONFIG_DIR.get().map(PathBuf::as_path)
}

pub(crate) fn keyring_service_or(default: &'static str) -> &'static str {
    KEYRING_SERVICE.get().map_or(default, String::as_str)
}

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum LauncherError {
    #[error("app support directory not found for this platform")]
    NoAppSupportDir,

    #[error("bundled asset not found: {path}")]
    BundledAssetNotFound { path: PathBuf },

    #[error("manifest not found at {path}")]
    ManifestNotFound { path: PathBuf },

    #[error("manifest parse error: {reason}")]
    ManifestParseError { reason: String },

    #[error("manifest validation error: {reason}")]
    ManifestValidation { reason: String },

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

impl serde::Serialize for LauncherError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

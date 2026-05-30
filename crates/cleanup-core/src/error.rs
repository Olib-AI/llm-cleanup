use thiserror::Error;

/// Errors that can occur anywhere in the cleaning pipeline. The cardinal rule is that we
/// never emit a corrupt file: on any error the caller writes nothing and exits non-zero.
#[derive(Debug, Error)]
pub enum CoreError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("unsupported format: {0}")]
    UnsupportedFormat(String),

    #[error("parse error: {0}")]
    Parse(String),

    #[error("encoding error: {0}")]
    Encoding(String),

    /// The post-edit structural self-check failed: cleaning would have changed document
    /// structure. We abort rather than risk corrupting the file.
    #[error("structural self-check failed: {0}")]
    SelfCheck(String),

    #[error("{0} is not yet implemented")]
    NotImplemented(String),
}

pub type Result<T> = std::result::Result<T, CoreError>;

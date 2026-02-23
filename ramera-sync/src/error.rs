use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Config parse error: {0}")]
    ConfigParse(String),

    #[error("Invalid CIDR: {0}")]
    InvalidCidr(String),

    #[error("Storage error: {0}")]
    Storage(String),

    #[error("Command error: {0}")]
    Command(String),

    #[error("Backblaze B2 error: {0}")]
    B2(String),
}

pub type Result<T> = std::result::Result<T, AppError>;

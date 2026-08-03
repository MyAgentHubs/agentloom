pub type Result<T> = std::result::Result<T, HarnessError>;

#[derive(Debug, thiserror::Error)]
pub enum HarnessError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    ShellUnavailable(String),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("missing required environment variable: {0}")]
    MissingEnv(&'static str),
    #[error("invalid config: {0}")]
    InvalidConfig(String),
    #[error("invalid goal change: {0}")]
    InvalidGoalChange(String),
    #[error("provider error: {0}")]
    Provider(String),
    #[error("runtime error: {0}")]
    Runtime(String),
    #[error("permission denied: {0}")]
    PermissionDenied(String),
}

//! Application error type, rendered as `{"error": "..."}` with a matching status code.

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};

#[derive(Debug)]
pub enum AppError {
    NotFound(String),
    BadRequest(String),
    Conflict(String),
    Internal(String),
}

pub type AppResult<T> = Result<T, AppError>;

impl AppError {
    pub fn not_found(what: impl std::fmt::Display) -> Self {
        AppError::NotFound(format!("{what} not found"))
    }
    pub fn bad(msg: impl Into<String>) -> Self {
        AppError::BadRequest(msg.into())
    }
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AppError::NotFound(m) | AppError::BadRequest(m) | AppError::Conflict(m) | AppError::Internal(m) => {
                f.write_str(m)
            }
        }
    }
}

impl std::error::Error for AppError {}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = match &self {
            AppError::NotFound(_) => StatusCode::NOT_FOUND,
            AppError::BadRequest(_) => StatusCode::BAD_REQUEST,
            AppError::Conflict(_) => StatusCode::CONFLICT,
            AppError::Internal(m) => {
                eprintln!("internal error: {m}");
                StatusCode::INTERNAL_SERVER_ERROR
            }
        };
        (status, Json(serde_json::json!({ "error": self.to_string() }))).into_response()
    }
}

impl From<rusqlite::Error> for AppError {
    fn from(e: rusqlite::Error) -> Self {
        match e {
            rusqlite::Error::QueryReturnedNoRows => AppError::NotFound("not found".into()),
            e => AppError::Internal(format!("database: {e}")),
        }
    }
}

impl From<r2d2::Error> for AppError {
    fn from(e: r2d2::Error) -> Self {
        AppError::Internal(format!("db pool: {e}"))
    }
}

impl From<std::io::Error> for AppError {
    fn from(e: std::io::Error) -> Self {
        AppError::Internal(format!("io: {e}"))
    }
}

impl From<serde_json::Error> for AppError {
    fn from(e: serde_json::Error) -> Self {
        AppError::Internal(format!("json: {e}"))
    }
}

impl From<tokio::task::JoinError> for AppError {
    fn from(e: tokio::task::JoinError) -> Self {
        AppError::Internal(format!("task: {e}"))
    }
}

//! Errors that cross the command boundary.
//!
//! A command error reaches the frontend as `{ code, message }`. `message` is
//! what the user reads; `code` lets the frontend react to a specific failure
//! (offer to overwrite a file that changed, say) without matching on text.
//! Errors that are plain strings convert with the generic code, so `?` keeps
//! working while call sites that matter are given codes of their own.

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AppError {
    pub code: &'static str,
    pub message: String,
}

impl AppError {
    /// No more specific code applies.
    pub const GENERIC: &'static str = "error";
    /// The remote file changed after it was opened for editing.
    pub const FILE_CHANGED: &'static str = "file_changed";

    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl From<String> for AppError {
    fn from(message: String) -> Self {
        Self::new(Self::GENERIC, message)
    }
}

impl From<&str> for AppError {
    fn from(message: &str) -> Self {
        Self::new(Self::GENERIC, message)
    }
}

pub type AppResult<T> = Result<T, AppError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_with_a_code_the_frontend_can_switch_on() {
        let error = AppError::new(AppError::FILE_CHANGED, "远程文件已被修改");
        assert_eq!(
            serde_json::to_value(&error).unwrap(),
            serde_json::json!({"code": "file_changed", "message": "远程文件已被修改"})
        );
        assert_eq!(AppError::from("失败".to_string()).code, AppError::GENERIC);
        assert_eq!(error.to_string(), "远程文件已被修改");
    }
}

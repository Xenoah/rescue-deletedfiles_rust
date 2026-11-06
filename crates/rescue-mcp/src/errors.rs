use rescue_core::{Error, OperationError};

#[derive(Debug)]
pub(super) struct ToolError(pub OperationError);
impl From<Error> for ToolError {
    fn from(error: Error) -> Self {
        Self(error.report("request"))
    }
}
impl From<std::io::Error> for ToolError {
    fn from(error: std::io::Error) -> Self {
        Error::from(error).into()
    }
}
impl From<serde_json::Error> for ToolError {
    fn from(error: serde_json::Error) -> Self {
        Error::from(error).into()
    }
}
impl From<String> for ToolError {
    fn from(message: String) -> Self {
        Self(OperationError {
            code: "INVALID_REQUEST".into(),
            message,
            retryable: false,
            affected_scope: "request".into(),
        })
    }
}
impl From<&str> for ToolError {
    fn from(message: &str) -> Self {
        message.to_owned().into()
    }
}
impl From<std::num::TryFromIntError> for ToolError {
    fn from(error: std::num::TryFromIntError) -> Self {
        error.to_string().into()
    }
}

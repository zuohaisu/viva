//! The office error taxonomy.
//!
//! G0 error semantics: every failure carries a stable, human-readable cause;
//! control-channel rejections carry a machine-readable [`envelope::RejectionReason`]
//! so a caller can distinguish "you are not trusted" from "your payload is too
//! large" from "this request was already handled".

use crate::foundation::envelope::RejectionReason;
use crate::foundation::ids::IdError;

#[derive(Debug, thiserror::Error)]
pub enum OfficeError {
    #[error("storage error: {0}")]
    Storage(#[from] rusqlite::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Id(#[from] IdError),
    #[error("validation: {0}")]
    Validation(String),
    #[error("migration error: {0}")]
    Migration(String),
    #[error("not found: {entity} `{id}`")]
    NotFound { entity: &'static str, id: String },
    #[error("control request `{request_id}` rejected: {reason}")]
    ControlRejected {
        request_id: String,
        reason: RejectionReason,
    },
}

pub type OfficeResult<T> = Result<T, OfficeError>;

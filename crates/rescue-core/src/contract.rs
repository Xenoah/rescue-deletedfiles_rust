//! Versioned machine-readable operation contracts shared by all frontends.
use crate::Error;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationError {
    pub code: String,
    pub message: String,
    /// A transient condition may permit another attempt with the same inputs.
    /// Never authorizes retrying a partially written output in place.
    pub retryable: bool,
    pub affected_scope: String,
}
impl Error {
    pub fn report(&self, affected_scope: impl Into<String>) -> OperationError {
        let retryable = match self {
            Self::Io(error) => matches!(
                error.kind(),
                std::io::ErrorKind::Interrupted
                    | std::io::ErrorKind::WouldBlock
                    | std::io::ErrorKind::TimedOut
                    | std::io::ErrorKind::StorageFull
            ),
            Self::Index(rusqlite::Error::SqliteFailure(error, _)) => matches!(
                error.code,
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
            ),
            _ => false,
        };
        OperationError {
            code: self.code().into(),
            message: self.to_string(),
            retryable,
            affected_scope: affected_scope.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    Scan,
    Imaging,
    Recovery,
    Discovery,
    RaidConfiguration,
    RaidSearch,
    Preview,
    ResultOrder,
    SessionMigration,
}
impl JobKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Scan => "scan",
            Self::Imaging => "imaging",
            Self::Recovery => "recovery",
            Self::Discovery => "discovery",
            Self::RaidConfiguration => "raid_configuration",
            Self::RaidSearch => "raid_search",
            Self::Preview => "preview",
            Self::ResultOrder => "result_order",
            Self::SessionMigration => "session_migration",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retry_contract_distinguishes_transient_conditions_from_changed_evidence() {
        for error in [
            Error::SourceChanged,
            Error::DestinationChanged,
            Error::Cancelled,
            Error::Invalid("bad length".into()),
            Error::Limit("pixels".into()),
            std::io::Error::from(std::io::ErrorKind::PermissionDenied).into(),
        ] {
            let detail = error.report("candidate:42");
            assert!(!detail.retryable);
            assert_eq!(detail.affected_scope, "candidate:42");
        }
        for kind in [
            std::io::ErrorKind::Interrupted,
            std::io::ErrorKind::TimedOut,
            std::io::ErrorKind::StorageFull,
        ] {
            let detail = Error::from(std::io::Error::from(kind)).report("image");
            assert!(detail.retryable);
            assert_eq!(detail.code, "IO_ERROR");
        }
    }
}

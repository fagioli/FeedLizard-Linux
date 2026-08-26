use std::{error::Error, fmt, io};

#[derive(Debug)]
pub enum StorageError {
    Open(String),
    Migration(String),
    Constraint(String),
    Transaction(String),
    Corruption(String),
    UnsupportedSchema(i64),
    Search(String),
    ImportExport(String),
    NotFound(&'static str),
    InvalidInput(&'static str),
}

impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Open(value) => write!(f, "database open failed: {value}"),
            Self::Migration(value) => write!(f, "database migration failed: {value}"),
            Self::Constraint(value) => write!(f, "database constraint failed: {value}"),
            Self::Transaction(value) => write!(f, "database transaction failed: {value}"),
            Self::Corruption(value) => write!(f, "database integrity failed: {value}"),
            Self::UnsupportedSchema(version) => {
                write!(f, "unsupported database schema version {version}")
            }
            Self::Search(value) => write!(f, "search failed: {value}"),
            Self::ImportExport(value) => write!(f, "import/export failed: {value}"),
            Self::NotFound(kind) => write!(f, "{kind} was not found"),
            Self::InvalidInput(kind) => write!(f, "invalid {kind}"),
        }
    }
}

impl Error for StorageError {}

pub(crate) fn sqlite(error: rusqlite::Error) -> StorageError {
    match &error {
        rusqlite::Error::SqliteFailure(code, _)
            if code.code == rusqlite::ErrorCode::ConstraintViolation =>
        {
            StorageError::Constraint(error.to_string())
        }
        rusqlite::Error::SqliteFailure(code, _)
            if code.code == rusqlite::ErrorCode::DatabaseCorrupt =>
        {
            StorageError::Corruption(error.to_string())
        }
        _ => StorageError::Transaction(error.to_string()),
    }
}

impl From<io::Error> for StorageError {
    fn from(value: io::Error) -> Self {
        Self::ImportExport(value.to_string())
    }
}

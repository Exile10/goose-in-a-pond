use thiserror::Error;

#[derive(Error, Debug)]
pub enum StorageError {
    #[error("General error: {0}")]
    General(String),
}

/// Driven Port: Storage
///
/// This trait defines the interface for Storage.
pub trait Storage: Send + Sync {
    // Define your methods here
}

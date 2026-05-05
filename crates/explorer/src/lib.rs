mod error;

pub use error::ExplorerError;

pub type Result<T> = std::result::Result<T, ExplorerError>;

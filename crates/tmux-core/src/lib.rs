mod error;

pub use error::TmuxError;

pub type Result<T> = std::result::Result<T, TmuxError>;

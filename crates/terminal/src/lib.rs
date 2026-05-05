mod error;

pub use error::TerminalError;

pub type Result<T> = std::result::Result<T, TerminalError>;

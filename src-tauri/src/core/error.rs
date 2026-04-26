use thiserror::Error;

#[derive(Debug, Error)]
pub enum ToolkitError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Invalid magic bytes: expected {expected:#010X}, got {got:#010X}")]
    InvalidMagic { expected: u32, got: u32 },

    #[error("Unknown game version: {0:#010X}")]
    UnknownVersion(u32),

    #[error("Section not found: tag {0:#010X}")]
    SectionNotFound(u32),

    #[error("Parse error: {0}")]
    Parse(String),

    #[error("ASCII format error at line {line}: {message}")]
    AsciiFormat { line: usize, message: String },

    #[error("Unsupported feature: {0}")]
    Unsupported(String),
}

pub type Result<T> = std::result::Result<T, ToolkitError>;

impl serde::Serialize for ToolkitError {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

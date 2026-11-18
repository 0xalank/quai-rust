//! Shared distinction between invalid execution input and unavailable local state.
/// A proved absence is different from an unavailable or corrupt local state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReadError {
    Invalid(String),
    Unavailable(String),
}
impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(v) => f.write_str(v),
            Self::Unavailable(v) => write!(f, "state unavailable: {v}"),
        }
    }
}
impl std::error::Error for ReadError {}
impl From<&str> for ReadError {
    fn from(v: &str) -> Self {
        Self::Invalid(v.into())
    }
}
impl From<String> for ReadError {
    fn from(v: String) -> Self {
        Self::Invalid(v)
    }
}
impl From<ReadError> for String {
    fn from(v: ReadError) -> String {
        v.to_string()
    }
}

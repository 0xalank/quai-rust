//! Fixed-width reads that return an error instead of panicking. Decoders use
//! these where a slice length comes from peer or file bytes.
use crate::Error;

pub(crate) struct Width;
impl From<Width> for Error {
    fn from(_: Width) -> Self {
        Error::Invalid("fixed-width field")
    }
}
impl From<Width> for std::io::Error {
    fn from(_: Width) -> Self {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "fixed-width field")
    }
}
/// Copy `bytes` into an array when its length is exactly `N`.
pub(crate) fn array<const N: usize>(bytes: &[u8]) -> Result<[u8; N], Width> {
    bytes.try_into().map_err(|_| Width)
}

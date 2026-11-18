//! Fixed-width reads that return an error instead of panicking. Decoders use
//! these where a slice length comes from local file or peer bytes.
pub(crate) struct Width;
impl From<Width> for String {
    fn from(_: Width) -> Self {
        "fixed-width field".into()
    }
}
impl From<Width> for std::io::Error {
    fn from(_: Width) -> Self {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "fixed-width field")
    }
}
impl From<Width> for crate::qi::ReadError {
    fn from(_: Width) -> Self {
        crate::qi::ReadError::Unavailable("fixed-width field".into())
    }
}
/// Copy `bytes` into an array when its length is exactly `N`.
pub(crate) fn array<const N: usize>(bytes: &[u8]) -> Result<[u8; N], Width> {
    bytes.try_into().map_err(|_| Width)
}
/// Copy the `N` bytes at offset `at`, or return `Width` when they are not all
/// there.
pub(crate) fn field<const N: usize>(bytes: &[u8], at: usize) -> Result<[u8; N], Width> {
    bytes
        .get(at..)
        .and_then(|b| b.first_chunk::<N>())
        .copied()
        .ok_or(Width)
}

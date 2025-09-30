use nix::errno::Errno;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ApplicationError {
    #[error("File I/O Error")]
    RawFileIoError(#[from] Errno),
    #[error("File I/O Error (Managed)")]
    ManagedFileIoError(#[from] std::io::Error),
    #[error("Unknown error")]
    UnknownError(&'static str),
}

use nix::errno::Errno;

pub(crate) fn wrap_ioctl_negative_invalid(result: Result<i32, Errno>) -> Result<i32, Errno> {
    match result {
        Ok(num) => match num {
            ..=-1 => Err(Errno::UnknownErrno),
            _ => Ok(num),
        },
        Err(e) => Err(e),
    }
}

pub(crate) fn u8_slice_to_string(slice: &[u8]) -> String {
    let len = slice.iter().position(|&b| b == 0).unwrap_or(slice.len());
    String::from_utf8_lossy(&slice[..len]).to_string()
}
use std::{
    fs::{self, File},
    io::Read,
    os::fd::{AsRawFd, OwnedFd},
    path::PathBuf,
};

use nix::{
    errno::Errno,
    fcntl::{OFlag, open},
    ioctl_write_ptr,
    sys::stat::Mode,
    unistd::{read, write},
};

use crate::error::ApplicationError;

const RPMSG_CTRL_DEV: &str = "/dev/rpmsg_ctrl0";
ioctl_write_ptr!(rpmsg_create_ept_ioctl, 0xb5, 0x1, RpmsgEndpointInfo);

#[repr(C)]
struct RpmsgEndpointInfo {
    name: [u8; 32],
    src: u32,
    dst: u32,
}

impl Default for RpmsgEndpointInfo {
    fn default() -> Self {
        let mut buf = [0u8; 32];
        buf[..11].copy_from_slice(b"msgbox_demo");

        RpmsgEndpointInfo {
            name: buf,
            src: 0x3,
            dst: 0xffffffff,
        }
    }
}

pub struct MsgboxEndpoint {
    msgbox_fd_ctrl: OwnedFd,
    msgbox_fd_ept: OwnedFd,
    pub msgbox_new_msg_read: u16,
    msgbox_new_msg_write: u16,
}

fn wrap_ioctl_negative_invalid(result: Result<i32, Errno>) -> Result<i32, Errno> {
    match result {
        Ok(num) => match num {
            ..=-1 => Err(Errno::UnknownErrno),
            _ => Ok(num),
        },
        Err(e) => Err(e),
    }
}

// TODO: Not very proud of this one
fn get_ept_interface_by_name(rpmsg_endpoint_info: &RpmsgEndpointInfo) -> Option<PathBuf> {
    // TODO: Find a better solution for this
    let name_len = rpmsg_endpoint_info
        .name
        .iter()
        .position(|&b| b == 0)
        .unwrap();
    let ept_name = unsafe { str::from_utf8_unchecked(&rpmsg_endpoint_info.name[..name_len]) };

    let directories = match fs::read_dir("/sys/class/rpmsg") {
        Ok(directories) => directories,
        Err(_) => return None,
    };

    for ele in directories {
        let ele = ele.unwrap();
        let ele_file_name = ele.file_name();
        let ele_name = ele_file_name.to_str().unwrap();
        if ele_name.starts_with("rpmsg") {
            let mut path = ele.path();
            path.push("name");

            let mut file = match File::open(&path) {
                Ok(f) => f,
                Err(_) => continue,
            };

            let mut buf = [0u8; 32];
            if file.read_exact(&mut buf[..name_len]).is_err() {
                continue;
            }

            // TODO: Find a better solution for this
            let buf_len = buf.iter().position(|&b| b == 0).unwrap();
            let buf_as_str = unsafe { str::from_utf8_unchecked(&buf[..buf_len]) };

            if buf_as_str == ept_name {
                return Some(PathBuf::from(format!("/dev/{}", ele_name)));
            }
        }
    }

    None
}

impl MsgboxEndpoint {
    pub fn new() -> Result<MsgboxEndpoint, ApplicationError> {
        let msgbox_fd_ctrl = open(RPMSG_CTRL_DEV, OFlag::O_RDWR, Mode::empty())?;

        let ept_info = RpmsgEndpointInfo::default();

        wrap_ioctl_negative_invalid(unsafe {
            rpmsg_create_ept_ioctl(msgbox_fd_ctrl.as_raw_fd(), &ept_info)
        })?;
        let ept_interface = match get_ept_interface_by_name(&ept_info) {
            Some(ept_interface) => ept_interface,
            None => {
                return Err(ApplicationError::UnknownError(
                    "Failed to find opened ept interface",
                ));
            }
        };

        let msgbox_fd_ept = open(&ept_interface, OFlag::O_RDWR, Mode::empty())?;

        println!("Opened msgbox!");

        Ok(MsgboxEndpoint {
            msgbox_fd_ctrl: msgbox_fd_ctrl,
            msgbox_fd_ept: msgbox_fd_ept,
            msgbox_new_msg_read: 0,
            msgbox_new_msg_write: 0,
        })
    }

    pub fn msgbox_read_signal(
        &mut self,
        sharespace_arm_addr_read: u16,
    ) -> Result<bool, ApplicationError> {
        let mut buf = [0u8; 4];
        let ret = read(&self.msgbox_fd_ept, &mut buf)?;

        if ret != 4 {
            println!("Warn: Read msgbox size is not 4, but {}", ret);
        }

        let data_recv = u32::from_le_bytes(buf[..].try_into().unwrap());

        self.msgbox_new_msg_read = data_recv as u16;
        self.msgbox_new_msg_write = (data_recv >> 16) as u16;

        println!(
            "Msgbox read signal: read {}, write {}",
            self.msgbox_new_msg_read, self.msgbox_new_msg_write
        );

        if self.msgbox_new_msg_write >= 5000 {
            return Ok(false);
        }

        if self.msgbox_new_msg_write == sharespace_arm_addr_read {
            return Ok(false);
        }

        Ok(true)
    }

    pub fn msgbox_send_signal(
        &mut self,
        sharespace_arm_addr_read: u16,
        sharespace_arm_addr_write: u16,
    ) -> Result<(), ApplicationError> {
        let data_send =
            ((sharespace_arm_addr_write as u32) << 16) | sharespace_arm_addr_read as u32;
        let a = write(&self.msgbox_fd_ept, &data_send.to_le_bytes()[..])?;

        println!("Wrote {} bytes ({:#x}) to msgbox", a, data_send);

        Ok(())
    }
}

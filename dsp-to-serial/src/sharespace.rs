use std::os::fd::{AsRawFd, OwnedFd};

use memmap2::{MmapMut, MmapOptions};
use nix::{errno::Errno, fcntl::{open, OFlag}, ioctl_readwrite_bad, sys::stat::Mode};

use crate::util::wrap_ioctl_negative_invalid;

#[repr(C)]
#[derive(Default, Debug)]
struct DebugMessage {
    pub sys_cnt: u32,
    pub log_head_addr: u32,
    pub log_end_addr: u32,
    pub log_head_size: u32,
}

#[repr(C)]
#[derive(Default, Debug)]
pub struct DspSharespace {
    pub dsp_write_addr: u32,
    pub dsp_write_size: u32,

    pub arm_write_addr: u32,
    pub arm_write_size: u32,

    pub dsp_log_addr: u32,
    pub dsp_log_size: u32,

    pub mmap_phy_addr: u32,
    pub mmap_phy_size: u32,

    pub arom_read_dsp_log_addr: u32,
    pub debug_msg: DebugMessage,
}

enum ChooseShareSpace {
    CHOOSE_DSP_WRITE_SPACE = 0,
    CHOOSE_ARM_WRITE_SPACE = 1,
}

ioctl_readwrite_bad!(read_debug_message, 0x01, DspSharespace);
ioctl_readwrite_bad!(write_debug_message, 0x03, DspSharespace);

fn choose_sharespace(
    fd: &OwnedFd,
    msg: &mut DspSharespace,
    choose: ChooseShareSpace,
) -> Result<(), Errno> {
    let raw_fd = fd.as_raw_fd();
    wrap_ioctl_negative_invalid(unsafe { read_debug_message(raw_fd, msg) })?;

    println!("Before choose: {:#?}", msg);

    msg.mmap_phy_addr = match choose {
        ChooseShareSpace::CHOOSE_DSP_WRITE_SPACE => msg.dsp_write_addr,
        ChooseShareSpace::CHOOSE_ARM_WRITE_SPACE => msg.arm_write_addr,
    };

    wrap_ioctl_negative_invalid(unsafe { write_debug_message(raw_fd, msg) })?;

    Ok(())
}

fn sharespace_open() -> Result<OwnedFd, Errno> {
    open(
        "/dev/dsp_debug",
        OFlag::O_RDWR | OFlag::O_SYNC | OFlag::O_NONBLOCK,
        Mode::empty(),
    )
}

pub struct Sharespace {
    fd: OwnedFd,
    pub dsp_sharespace: DspSharespace,
    pub write_buffer: MmapMut, // ARM buffer - pu8ArmBuf
}

pub fn sharespace_mmap() -> Sharespace {
    let mut dsp_sharespace = DspSharespace::default();
    let fd = sharespace_open().unwrap();

    choose_sharespace(
        &fd,
        &mut dsp_sharespace,
        ChooseShareSpace::CHOOSE_ARM_WRITE_SPACE,
    )
    .unwrap();

    let write_buffer = unsafe { MmapOptions::new().len(0x1000).map_mut(&fd).unwrap() };

    Sharespace {
        fd,
        dsp_sharespace,
        write_buffer,
    }
}
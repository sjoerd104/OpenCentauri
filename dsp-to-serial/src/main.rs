use std::{ffi::c_void, fs::File, os::fd::{AsRawFd, OwnedFd, RawFd}, thread::Thread, time::Duration};
use nix::{errno::Errno, fcntl::{open, OFlag}, ioctl_read_bad, ioctl_readwrite, ioctl_readwrite_bad, ioctl_write_ptr_bad, libc::{self, ioctl, msync, MS_INVALIDATE}, sys::stat::Mode};

use memmap2::{Mmap, MmapMut, MmapOptions};

#[repr(C)]
#[derive(Default, Debug)]
struct DebugMessage {
    pub sys_cnt : u32,
    pub log_head_addr : u32,
    pub log_end_addr : u32,
    pub log_head_size : u32,
}

#[repr(C)]
#[derive(Default, Debug)]
struct DspSharespace {
    pub dsp_write_addr : u32,
    pub dsp_write_size : u32,

    pub arm_write_addr : u32,
    pub arm_write_size : u32,

    pub dsp_log_addr : u32,
    pub dsp_log_size : u32,

    pub mmap_phy_addr : u32,
    pub mmap_phy_size : u32,

    pub arom_read_dsp_log_addr : u32,
    pub debug_msg : DebugMessage,
}

enum ChooseShareSpace 
{
    CHOOSE_DSP_WRITE_SPACE = 0,
    CHOOSE_ARM_WRITE_SPACE = 1,
    CHOOSE_DSP_LOG_SPACE = 2,
}

ioctl_readwrite_bad!(read_debug_message, 0x01, DspSharespace);
ioctl_readwrite_bad!(write_debug_message, 0x03, DspSharespace);
ioctl_readwrite_bad!(kbuf_mgr_dev_create_buf, 0x100, KBufBufData);

fn choose_sharespace(fd : &OwnedFd, msg : &mut DspSharespace, choose : ChooseShareSpace) -> Result<(), Errno>
{
    let raw_fd = fd.as_raw_fd();
    wrap_ioctl_negative_invalid(unsafe { read_debug_message(raw_fd, msg) })?;

    println!("Before choose: {:#?}", msg);

    msg.mmap_phy_addr = match choose 
    {
        ChooseShareSpace::CHOOSE_DSP_WRITE_SPACE => msg.dsp_write_addr,
        ChooseShareSpace::CHOOSE_ARM_WRITE_SPACE => msg.arm_write_addr,
        ChooseShareSpace::CHOOSE_DSP_LOG_SPACE => msg.dsp_log_addr,
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

struct Sharespace 
{
    fd: OwnedFd,
    dsp_sharespace : DspSharespace,
    read_buffer : Mmap, // DSP buffer
    write_buffer : MmapMut // ARM buffer
}

fn sharespace_mmap() -> Sharespace
{
    let mut dsp_sharespace = DspSharespace::default();
    let fd = sharespace_open().unwrap();

    choose_sharespace(&fd, &mut dsp_sharespace, ChooseShareSpace::CHOOSE_ARM_WRITE_SPACE).unwrap();
    
    let write_buffer = unsafe {
        MmapOptions::new().len(0x1000).map_mut(&fd).unwrap()
    };

    choose_sharespace(&fd, &mut dsp_sharespace, ChooseShareSpace::CHOOSE_DSP_WRITE_SPACE).unwrap();

    let read_buffer = unsafe {
        MmapOptions::new().len(0x1000).map(&fd).unwrap()
    };

    Sharespace {
        fd,
        dsp_sharespace,
        read_buffer,
        write_buffer
    }
}

#[repr(C)]
#[derive(Debug)]
struct KBufBufData
{
    name : [u8; 32],
    len : u32,
    ktype : u32,
    minor : i32,
    va : u32, // ptr, virtual address that program can directly use.
    pa : u32, // ptr, physical address.
}

struct UserWrapperBufData
{
    buf : KBufBufData,
    mgr_fd: OwnedFd,
    map_fd : OwnedFd,
    addr : MmapMut,
}

impl Default for KBufBufData
{
    fn default() -> Self {
        let mut buf = [0u8; 32];
        buf[..4].copy_from_slice(b"test");

        Self { 
            name: buf, 
            len: 4 * 4096, 
            ktype: 1, // KBUF_TYPE_NONCACHE, 
            minor: Default::default(), 
            va: Default::default(), 
            pa: Default::default() 
        }
    }
}

fn wrap_ioctl_negative_invalid(result : Result<i32, Errno>) -> Result<i32, Errno>
{
    match result
    {
        Ok(num) => match num {
            ..=-1 => Err(Errno::UnknownErrno),
            _ => Ok(num)
        },
        Err(e) => Err(e)
    }
}

fn kbuf_use_new_buf(arm_write_addr : u32) -> Result<UserWrapperBufData, Errno>
{
    let mut buf_data = KBufBufData::default(); // Maybe should be dynamic?
    buf_data.pa = arm_write_addr;

    let mgr_fd = open(
        "/dev/kbuf-mgr-0",
        OFlag::O_RDWR,
        Mode::empty(),
    ).expect("Failed to open kbuf manager device"); // Todo: include error type for this

    let mgr_fd_raw = mgr_fd.as_raw_fd();

    println!("{:#?}", &buf_data);

    let ret = unsafe {
        wrap_ioctl_negative_invalid(kbuf_mgr_dev_create_buf(mgr_fd_raw, &mut buf_data))?
    };

    let name_len = buf_data.name.iter().position(|&b| b == 0).unwrap_or(buf_data.name.len());
    let map_dev_path = format!("/dev/kbuf-map-{}-{}", buf_data.minor, unsafe { str::from_utf8_unchecked(&buf_data.name[..name_len]) });

    println!("Mapping kbuf device at path: {}", map_dev_path);

    let map_fd = open(
        map_dev_path.as_str(),
        OFlag::O_RDWR,
        Mode::empty(),
    ).expect("Failed to open map device"); // Todo: include error type for this

    let addr = unsafe {
        MmapOptions::new().len(buf_data.len as usize).map_mut(&map_fd).unwrap()
    };

    Ok(UserWrapperBufData {
        buf: buf_data,
        mgr_fd,
        map_fd,
        addr
    })
}

#[repr(C)]
#[derive(Default, Debug)]
struct MsgHead
{
    read_addr : u32,
    write_addr : u32,
    init_state : u32,
}

impl MsgHead 
{
    fn from_bytes(bytes : &[u8; 12]) -> Self 
    {
        let read_addr = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
        let write_addr = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
        let init_state = u32::from_le_bytes(bytes[8..12].try_into().unwrap());

        MsgHead {
            read_addr,
            write_addr,
            init_state
        }
    }

    fn to_bytes(&self) -> [u8; 12]
    {
        let mut bytes = [0u8; 12];
        bytes[0..4].copy_from_slice(&self.read_addr.to_le_bytes());
        bytes[4..8].copy_from_slice(&self.write_addr.to_le_bytes());
        bytes[8..12].copy_from_slice(&self.init_state.to_le_bytes());
        bytes
    }
}

struct CommunicationHandler
{
    sharespace : Sharespace,
    user_buf : UserWrapperBufData,
    arm_head : MsgHead,
    dsp_head : MsgHead,
    sharespace_arm_addr_read : u32,
    sharespace_arm_addr_write : u32,
    msgbox_send_addr_read : u32,
    msgbox_send_addr_write : u32,
}

const SHARE_SPACE_HEAD_OFFSET : usize = 4096 - size_of::<MsgHead>();

impl CommunicationHandler
{
    fn new(sharespace: Sharespace, user_buf: UserWrapperBufData) -> Self
    {
        let mut arm_head = MsgHead::default();
        let dsp_head = MsgHead::default();

        arm_head.read_addr = size_of::<MsgHead>() as u32;
        arm_head.write_addr = size_of::<MsgHead>() as u32;
        arm_head.init_state = 1;

        let mut communication_handler = CommunicationHandler{
            sharespace,
            user_buf,
            arm_head,
            dsp_head,
            sharespace_arm_addr_read: size_of::<MsgHead>() as u32,
            sharespace_arm_addr_write: size_of::<MsgHead>() as u32,
            msgbox_send_addr_read: size_of::<MsgHead>() as u32,
            msgbox_send_addr_write: size_of::<MsgHead>() as u32,
        };

        communication_handler.write_arm_head();

        communication_handler 
    }

    fn init_no_mmap(&mut self)
    {
        let mut head = MsgHead::from_bytes(self.sharespace.write_buffer.as_ref()[SHARE_SPACE_HEAD_OFFSET..].try_into().unwrap());

        head.init_state = if head.init_state == 1 || head.init_state == 2 { 2 } else { 1 };
        head.read_addr = self.sharespace.dsp_sharespace.arm_write_addr + 4096;
        head.write_addr = self.sharespace.dsp_sharespace.arm_write_addr;

        self.sharespace.write_buffer.as_mut()[SHARE_SPACE_HEAD_OFFSET..]
            .copy_from_slice(&head.to_bytes())
    }

    fn get_write_slice(&mut self) -> &mut [u8]
    {
        &mut self.user_buf.addr.as_mut()[0..4096]
    }

    fn get_read_slice(&self) -> &[u8]
    {
        &self.user_buf.addr.as_ref()[4096..8192]
    }

    fn read_dsp_head(&mut self)
    {
        self.dsp_head = MsgHead::from_bytes(self.get_read_slice()[SHARE_SPACE_HEAD_OFFSET..].try_into().unwrap())
    }

    fn write_arm_head(&mut self)
    {
        let bytes= self.arm_head.to_bytes();
        
        self.get_write_slice()[SHARE_SPACE_HEAD_OFFSET..]
            .copy_from_slice(&bytes)
    }

    unsafe fn invalidate_read_buffer(&mut self)
    {
        msync(self.user_buf.addr.as_mut_ptr().add(4096) as *mut c_void, 4096, MS_INVALIDATE);
    }

    fn wait_dsp_set_init(&mut self)
    {
        self.arm_head.read_addr = size_of::<MsgHead>() as u32;
        self.arm_head.write_addr = size_of::<MsgHead>() as u32;
        self.arm_head.init_state = 1;

        loop {
            unsafe { self.invalidate_read_buffer() };
            self.read_dsp_head();
            self.write_arm_head();
            
            if self.dsp_head.init_state == 1
            {
                println!("Yay!");
                self.sharespace_arm_addr_read = self.dsp_head.read_addr;
                self.sharespace_arm_addr_write = self.dsp_head.write_addr;
                break;
            }

            println!("Arm head: {:#?}", self.arm_head);

                    println!(
            "{}",
            self.get_write_slice().iter()
                .map(|b| format!("{:02X}", b))
                .collect::<Vec<String>>()
                .join("")
        );

            println!("Dsp head: {:#?}", self.dsp_head);



                println!(
            "{}",
            self.get_read_slice().iter()
                .map(|b| format!("{:02X}", b))
                .collect::<Vec<String>>()
                .join("")
        );

            std::thread::sleep(Duration::from_micros(10000));
        }
    }
}

fn main() {
    println!("Hello, world!");
    let mmap = sharespace_mmap();
    println!("Got sharespace mmap!");
    let kbuf = kbuf_use_new_buf(mmap.dsp_sharespace.arm_write_addr).unwrap();
    println!("Got kbuf mmap!");
    let mut handler = CommunicationHandler::new(mmap, kbuf);
    println!("Got communication handler!");
    handler.init_no_mmap();
    println!("Done init_no_mmap!");
    handler.wait_dsp_set_init();
    println!("Done init!");
}

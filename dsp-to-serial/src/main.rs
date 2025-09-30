use nix::{
    errno::Errno,
    fcntl::{OFlag, open},
    ioctl_readwrite_bad,
    libc::{MS_INVALIDATE, msync},
    sys::stat::Mode,
};
use std::{
    ffi::c_void,
    os::fd::{AsRawFd, OwnedFd},
    time::Duration,
};

use memmap2::{MmapMut, MmapOptions};

use crate::{
    kbuf::{UserWrapperBufData, kbuf_use_new_buf},
    msgbox::MsgboxEndpoint,
    sharespace::{Sharespace, sharespace_mmap},
};

mod error;
mod kbuf;
mod msgbox;
mod sharespace;
mod util;

#[repr(C)]
#[derive(Default, Debug)]
struct MsgHead {
    read_addr: u32,
    write_addr: u32,
    init_state: u32,
}

impl MsgHead {
    fn from_bytes(bytes: &[u8; 12]) -> Self {
        let read_addr = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
        let write_addr = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
        let init_state = u32::from_le_bytes(bytes[8..12].try_into().unwrap());

        MsgHead {
            read_addr,
            write_addr,
            init_state,
        }
    }

    fn to_bytes(&self) -> [u8; 12] {
        let mut bytes = [0u8; 12];
        bytes[0..4].copy_from_slice(&self.read_addr.to_le_bytes());
        bytes[4..8].copy_from_slice(&self.write_addr.to_le_bytes());
        bytes[8..12].copy_from_slice(&self.init_state.to_le_bytes());
        bytes
    }
}

struct CommunicationHandler {
    sharespace: Sharespace,
    pub user_buf: UserWrapperBufData,
    arm_head: MsgHead,
    dsp_head: MsgHead,
}

const SHARE_SPACE_HEAD_OFFSET: usize = 4096 - size_of::<MsgHead>();
const MIN_ADDR: usize = size_of::<MsgHead>();
const MAX_ADDR: usize = SHARE_SPACE_HEAD_OFFSET;

impl CommunicationHandler {
    fn new(sharespace: Sharespace, user_buf: UserWrapperBufData) -> Self {
        let mut arm_head = MsgHead::default();
        let dsp_head = MsgHead::default();

        arm_head.read_addr = size_of::<MsgHead>() as u32;
        arm_head.write_addr = size_of::<MsgHead>() as u32;
        arm_head.init_state = 1;

        let mut communication_handler = CommunicationHandler {
            sharespace,
            user_buf,
            arm_head,
            dsp_head,
        };

        communication_handler.write_arm_head();

        communication_handler
    }

    fn init_no_mmap(&mut self) {
        let mut head = MsgHead::from_bytes(
            self.sharespace.write_buffer.as_ref()[SHARE_SPACE_HEAD_OFFSET..]
                .try_into()
                .unwrap(),
        );

        head.init_state = if head.init_state == 1 || head.init_state == 2 {
            2
        } else {
            1
        };
        head.read_addr = self.user_buf.buf.pa + 4096;
        head.write_addr = self.user_buf.buf.pa;

        self.sharespace.write_buffer.as_mut()[SHARE_SPACE_HEAD_OFFSET..]
            .copy_from_slice(&head.to_bytes())
    }

    // pVirArmBuf
    fn get_write_slice(&mut self) -> &mut [u8] {
        &mut self.user_buf.addr.as_mut()[0..4096]
    }

    // pVirDspBuf
    fn get_read_slice(&self) -> &[u8] {
        &self.user_buf.addr.as_ref()[4096..8192]
    }

    fn read_dsp_head(&mut self) {
        self.dsp_head = MsgHead::from_bytes(
            self.get_read_slice()[SHARE_SPACE_HEAD_OFFSET..]
                .try_into()
                .unwrap(),
        )
    }

    fn debug_read_dsp_head(&mut self) {
        let dsp_head = MsgHead::from_bytes(
            self.get_read_slice()[SHARE_SPACE_HEAD_OFFSET..]
                .try_into()
                .unwrap(),
        );
        println!("DSP head in memory: {:?}", dsp_head);
    }

    fn write_arm_head(&mut self) {
        let bytes = self.arm_head.to_bytes();

        self.get_write_slice()[SHARE_SPACE_HEAD_OFFSET..].copy_from_slice(&bytes)
    }

    fn debug_read_arm_head(&mut self) {
        let arm_head = MsgHead::from_bytes(
            self.get_write_slice()[SHARE_SPACE_HEAD_OFFSET..]
                .try_into()
                .unwrap(),
        );
        println!("ARM head in memory: {:?}", arm_head);
    }

    unsafe fn invalidate_read_buffer(&mut self) {
        unsafe {
            msync(
                self.user_buf.addr.as_mut_ptr().add(4096) as *mut c_void,
                4096,
                MS_INVALIDATE,
            );
        }
    }

    fn wait_dsp_set_init(&mut self) {
        self.arm_head.read_addr = size_of::<MsgHead>() as u32;
        self.arm_head.write_addr = size_of::<MsgHead>() as u32;
        self.arm_head.init_state = 1;

        loop {
            unsafe { self.invalidate_read_buffer() };
            self.read_dsp_head();
            self.write_arm_head();

            println!("Arm head: {:#?}", self.arm_head);
            println!("Dsp head: {:#?}", self.dsp_head);

            if self.dsp_head.init_state == 1 {
                println!("Yay!");
                break;
            }

            std::thread::sleep(Duration::from_micros(10000));
        }
    }

    fn dsp_mem_read(&mut self) -> Vec<u8> {
        self.read_dsp_head();

        if self.arm_head.read_addr == self.dsp_head.write_addr {
            return vec![];
        }

        let mut msg_start_addr: usize = self.arm_head.read_addr as usize;
        let msg_size: usize;

        if self.arm_head.read_addr < self.dsp_head.write_addr {
            msg_size = (self.dsp_head.write_addr - self.arm_head.read_addr) as usize;
        } else {
            msg_size = MAX_ADDR
                - MIN_ADDR
                - ((self.arm_head.read_addr - self.dsp_head.write_addr) as usize);
        }

        let mut result;

        if msg_start_addr + msg_size <= MAX_ADDR {
            result = self.get_read_slice()[msg_start_addr..msg_start_addr + msg_size].to_vec();

            msg_start_addr += msg_size;

            if msg_start_addr >= MAX_ADDR {
                msg_start_addr = MIN_ADDR;
            }
        } else {
            let len1 = MAX_ADDR - msg_start_addr;
            result = self.get_read_slice()[msg_start_addr..msg_start_addr + len1].to_vec();
            result.extend(self.get_read_slice()[MIN_ADDR..MIN_ADDR + msg_size - len1].to_vec());
            msg_start_addr = MIN_ADDR + msg_size - len1;
        }

        if msg_size > 0 {
            self.arm_head.read_addr = msg_start_addr as u32;
        }

        return result;
    }

    fn dsp_mem_write(&mut self, msgbox_endpoint: &mut MsgboxEndpoint, data: &[u8]) {
        let mut len = data.len();

        if len > 4000 || len <= 0 {
            panic!("Cannot send too much or nothing!");
        }

        // Check: Can we not get the dsp head here?
        //self.debug_read_dsp_head();
        //self.dsp_head.read_addr = msgbox_endpoint.msgbox_new_msg_read as u32;
        self.read_dsp_head();
        println!("Local DSP head: {:?}", self.dsp_head);
        let free_size;

        if self.dsp_head.read_addr <= self.arm_head.write_addr {
            free_size =
                MAX_ADDR - MIN_ADDR - (self.arm_head.write_addr - self.dsp_head.read_addr) as usize;
            if free_size <= len {
                panic!("Good job");
            }
        } else {
            free_size = (self.dsp_head.read_addr - self.arm_head.write_addr) as usize;
            if free_size <= len {
                panic!("Good job");
            }
        }

        let mut pmsg = self.arm_head.write_addr as usize;
        if pmsg + len <= MAX_ADDR {
            self.get_write_slice()[pmsg..pmsg + len].copy_from_slice(data);
            pmsg += len;
            if pmsg >= MAX_ADDR {
                pmsg = MIN_ADDR;
            }
        } else {
            let len1 = MAX_ADDR - self.arm_head.write_addr as usize;
            self.get_write_slice()[pmsg..pmsg + len1].copy_from_slice(&data[..len1]);
            len -= len1;
            self.get_write_slice()[MIN_ADDR..MIN_ADDR + len].copy_from_slice(&data[len1..]);
            pmsg = MIN_ADDR + len;
        }

        self.arm_head.write_addr = pmsg as u32;
        self.arm_head.init_state = 1;
        self.write_arm_head();

        msgbox_endpoint
            .msgbox_send_signal(
                self.arm_head.read_addr as u16,
                self.arm_head.write_addr as u16,
            )
            .unwrap();

        println!("New write addr: {}", self.arm_head.write_addr);
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

    let mut msgbox = MsgboxEndpoint::new().unwrap();

    let mut i = 1;

    loop {
        if msgbox.msgbox_has_signal() {
            if msgbox
                .msgbox_read_signal(handler.arm_head.read_addr as u16)
                .is_ok()
            {
                std::thread::sleep(Duration::from_millis(100));
                println!("Got signal!");
                let data = handler.dsp_mem_read();

                println!(
                    "{}",
                    data.iter()
                        .map(|b| format!("{:02X}", b))
                        .collect::<Vec<String>>()
                        .join("")
                );
            } else {
                println!("Got error, probably nothing to read...");
            }
        } else {
            println!("No signal");
        }

        if i % 3 == 0 {
            println!("Testing write");
            let data: [u8; 10] = [0x04, 0x04, 0x7e, 0x7e, 0x7e, 0x7e, 0x7e, 0x7e, 0x7e, 0x7e];
            //let data = b"Hello from ARM!";
            handler.dsp_mem_write(&mut msgbox, &data[..]);
        }

        //handler.debug_read_arm_head();

        println!("Sleeping");
        std::thread::sleep(Duration::from_secs(1));
        i += 1;
    }
}

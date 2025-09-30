use std::{
    fs::remove_file, io::{ErrorKind, Read, Write}, os::unix::fs::symlink, time::{Duration, Instant}
};

use serialport::{SerialPort, TTYPort};

use crate::{
    communication_handler::CommunicationHandler, kbuf::{kbuf_use_new_buf}, msgbox::MsgboxEndpoint, sharespace::{sharespace_mmap}
};

mod error;
mod kbuf;
mod msgbox;
mod sharespace;
mod util;
mod communication_handler;

fn read_dsp(msgbox : &mut MsgboxEndpoint, handler : &mut CommunicationHandler, port: &mut TTYPort) {
    if !msgbox.msgbox_has_signal()
    {
        return;
    }

    let now = Instant::now();

    let new_data_to_read = match msgbox
        .msgbox_read_signal(handler.arm_head.read_addr as u16) {
            Ok(n) => n,
            Err(e) => {
                println!("Failed to read signal from msgbox: {}", e);
                return;
            }
        };

    if !new_data_to_read
    {
        println!("Got msgbox message but no data to read?");
        return;
    }

    let data = handler.dsp_mem_read();

    if data.len() <= 0
    {
        println!("No data available to read...");
        return;
    }

    
    port.write_all(&data).unwrap(); // TODO: Erorr handling
    println!("Read {} bytes from the DSP in {}ms.", data.len(), now.elapsed().as_millis());
}

fn write_dsp(msgbox : &mut MsgboxEndpoint, handler : &mut CommunicationHandler, port: &mut TTYPort)
{
    let now = Instant::now();
    let mut buff = [0u8; 4096];
    let len = match port.read(&mut buff) {
        Ok(l) => l,
        Err(ref e) if e.kind() == ErrorKind::TimedOut => {
            return;
        }
        Err(e) => {
            println!("Error reading from serial port: {}", e);
            panic!();
        }
    };

    handler.dsp_mem_write(msgbox, &buff[..len]);
    println!("Wrote {} bytes to the DSP in {}ms.", len, now.elapsed().as_millis());
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
    println!("Done DSP init!");
    let mut msgbox = MsgboxEndpoint::new().unwrap();
    println!("Got msgbox endpoint!");

    let (mut master, slave) = TTYPort::pair().expect("Unable to create ptty pair");
    master.set_timeout(Duration::ZERO).unwrap();

    let mut link_path = std::env::temp_dir();
    link_path.push("dsp-serial");

    let _ = remove_file(&link_path);
    let name = slave.name().unwrap();
    symlink(name, &link_path).unwrap();

    println!("Created serial port at {:?}", link_path);

    loop {
        read_dsp(&mut msgbox, &mut handler, &mut master);
        write_dsp(&mut msgbox, &mut handler, &mut master);
        std::thread::sleep(Duration::from_millis(10));
    }
}

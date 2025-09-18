use std::io::{Read, Write};
use std::process::exit;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serialport::TTYPort;

#[derive(Clone)]
pub struct SerialConnectionSettings {
    pub baud_rate: u32,
    pub device_path: String,
}

pub struct DataBlock {
    pub id: u8,
    pub data: Vec<u8>,
}

pub struct SerialPortManager {
    pub settings: Option<SerialConnectionSettings>,
    port: TTYPort,
    index: usize,
}

// Multiplexer port -> Serial port
pub struct SerialConnectionSenderProcessor {
    pub id: u8,
    pub port_manager: Arc<Mutex<SerialPortManager>>,
    pub port_receiver: Receiver<DataBlock>,
}

// Serial port -> Multiplexer port
pub struct SerialConnectionReceiverProcessor {
    pub id: u8,
    pub port_manager: Arc<Mutex<SerialPortManager>>,
    pub write_to_main_bus: Sender<DataBlock>,
}

pub struct SerialConnectionSender {
    pub id: u8,
    pub port_sender: Sender<DataBlock>,
}

impl SerialPortManager {
    pub fn with_settings(settings: SerialConnectionSettings) -> Self {
        let port = match serialport::new(&settings.device_path, settings.baud_rate)
            .timeout(Duration::MAX)
            .open_native()
        {
            Ok(port) => port,
            Err(e) => {
                eprintln!(
                    "Failed to open serial port {}: {}",
                    &settings.device_path, e
                );
                exit(4);
            }
        };

        SerialPortManager {
            settings: Some(settings),
            port: port,
            index: 0,
        }
    }

    pub fn with_port(port: TTYPort) -> Self {
        SerialPortManager {
            settings: None,
            port: port,
            index: 0,
        }
    }

    pub fn give_port(&mut self) -> TTYPort {
        #[cfg(debug_assertions)]
        println!("Giving port with index: {}", self.index);

        if self.index >= 2 {
            self.generate_new_set_of_ports();
        }

        let port = self
            .port
            .try_clone_native()
            .expect("Failed to clone serial port");
        self.index += 1;
        port
    }

    fn generate_new_set_of_ports(&mut self) {
        let settings = self
            .settings
            .clone()
            .expect("Serial connection settings is unavailable, cannot recreate serial port");

        loop {
            // TODO: Drop old serial port before opening new one.
            let serial_port = match serialport::new(&settings.device_path, settings.baud_rate)
                .timeout(Duration::MAX)
                .open_native()
            {
                Ok(port) => port,
                Err(e) => {
                    eprintln!(
                        "Failed to open serial port {}: {}. Waiting 100ms.",
                        settings.device_path, e
                    );
                    std::thread::sleep(Duration::from_millis(100));
                    continue;
                }
            };

            self.port = serial_port;
            break;
        }

        self.index = 0;
    }
}

impl SerialConnectionReceiverProcessor {
    pub fn process_loop(&self) {
        let mut read_port = give_port(&self.port_manager);

        #[cfg(debug_assertions)]
        println!("Starting receiver loop for port ID: {}", self.id);

        loop {
            let mut buffer = [0u8; 255];
            // TODO: Maybe combine read blocks so we don't spam the buffer with 1 byte read's?

            let bytes = match read_port.read(&mut buffer) {
                Ok(bytes) => bytes,
                Err(e) => {
                    eprintln!(
                        "Error reading from serial port {} in receiver loop: {}. Attempting to reconnect...",
                        self.id, e
                    );
                    read_port = give_port(&self.port_manager);
                    continue;
                }
            };

            let block = DataBlock {
                id: self.id,
                data: buffer[..bytes].to_vec(),
            };

            self.write_to_main_bus
                .send(block)
                .expect("Failed to send data block to main bus");
        }
    }
}

impl SerialConnectionSenderProcessor {
    pub fn process_loop(&self) {
        let mut write_port = give_port(&self.port_manager);

        #[cfg(debug_assertions)]
        println!("Starting sender loop for port ID: {}", self.id);

        loop {
            let block = self
                .port_receiver
                .recv()
                .expect("Failed to receive data block");

            loop {
                if let Err(e) = write_port.write_all(&block.data) {
                    eprintln!(
                        "Error writing to serial port {} in sender loop: {}. Attempting to reconnect...",
                        self.id, e
                    );
                    write_port = give_port(&self.port_manager);
                    continue;
                }

                break;
            }
        }
    }
}

pub fn give_port(port_manager: &Arc<Mutex<SerialPortManager>>) -> TTYPort {
    port_manager
        .lock()
        .expect("Failed to lock port manager")
        .give_port()
}

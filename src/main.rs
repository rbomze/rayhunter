use std::fs::File;
use std::io::Cursor;
use std::io::Read;
use std::io::Write;
use std::mem;
use std::io;
use std::net::TcpListener;
use std::sync::mpsc;
use bytes::{Buf, BufMut};
use std::os::fd::AsRawFd;
use std::thread;
use thiserror::Error;

type DiagResult<T> = Result<T, DiagDeviceError>;

const BUFFER_LEN: usize = 1024 * 1024 * 10;
const USER_SPACE_DATA_TYPE: i32 = 32;
const DIAG_IOCTL_REMOTE_DEV: u32 = 32;
const MEMORY_DEVICE_MODE: i32 = 2;
const DIAG_IOCTL_SWITCH_LOGGING: u32 = 7;

#[derive(Error, Debug)]
enum DiagDeviceError {
    #[error("IO error {0}")]
    IO(#[from] io::Error),
    #[error("Failed to initialize /dev/diag: {0}")]
    InitializationFailed(String),
    #[error("Failed to read diag device: {0}")]
    DeviceReadFailed(String),
}

struct DiagDevice {
    pub file: File,
    use_mdm: i32,
}

fn enable_frame_readwrite(fd: i32, mode: i32) -> DiagResult<()> {
    unsafe {
        if libc::ioctl(fd, DIAG_IOCTL_SWITCH_LOGGING, mode, 0, 0, 0) < 0 {
            let ret = libc::ioctl(
                fd,
                DIAG_IOCTL_SWITCH_LOGGING,
                &mut [mode, -1, 0] as *mut _, // diag_logging_mode_param_t
                mem::size_of::<[i32; 3]>(), 0, 0, 0, 0
            );
            if ret < 0 {
                let msg = format!("DIAG_IOCTL_SWITCH_LOGGING ioctl failed with error code {}", ret);
                return Err(DiagDeviceError::InitializationFailed(msg))
            }
        }
    }
    Ok(())
}

fn determine_use_mdm(fd: i32) -> DiagResult<i32> {
    let use_mdm: i32 = 0;
    unsafe {
        if libc::ioctl(fd, DIAG_IOCTL_REMOTE_DEV, &use_mdm as *const i32) < 0 {
            let msg = format!("DIAG_IOCTL_REMOTE_DEV ioctl failed with error code {}", 0);
            return Err(DiagDeviceError::InitializationFailed(msg))
        }
    }
    Ok(use_mdm)
}

impl DiagDevice {
    pub fn new() -> DiagResult<Self> {
        let file = File::options()
            .read(true)
            .write(true)
            .open("/dev/diag")?;
        let fd = file.as_raw_fd();

        enable_frame_readwrite(fd, MEMORY_DEVICE_MODE)?;
        let use_mdm = determine_use_mdm(fd)?;

        Ok(DiagDevice {
            file,
            use_mdm,
        })
    }

    pub fn try_clone(&self) -> DiagResult<Self> {
        Ok(DiagDevice {
            file: self.file.try_clone()?,
            use_mdm: self.use_mdm,
        })
    }

    pub fn read_response(&mut self) -> DiagResult<Option<Vec<Vec<u8>>>> {
        let mut buf = vec![0; BUFFER_LEN];
        let bytes_read = self.file.read(&mut buf)?;
        if bytes_read < 4 {
            let msg = format!("read {} bytes from diag device, expected > 4", bytes_read);
            return Err(DiagDeviceError::DeviceReadFailed(msg));
        }
        let mut reader = Cursor::new(buf);

        if reader.get_i32_le() != USER_SPACE_DATA_TYPE {
            return Ok(None);
        }

        let num_messages = reader.get_u32_le();
        let mut messages = Vec::new();

        for _ in 0..num_messages {
            let msg_len = reader.get_u32_le() as usize;
            let mut msg = vec![0; msg_len];
            reader.read_exact(&mut msg)?;
            messages.push(msg);
        }

        Ok(Some(messages))
    }

    pub fn write_request(&mut self, req: &[u8]) -> DiagResult<()> {
        let mut buf: Vec<u8> = vec![];
        buf.put_i32_le(USER_SPACE_DATA_TYPE);
        if self.use_mdm > 0 {
            buf.put_i32_le(-1);
        }
        buf.extend_from_slice(req);
        unsafe {
            let fd = self.file.as_raw_fd();
            let buf_ptr = buf.as_ptr() as *const libc::c_void;
            let ret = libc::write(fd, buf_ptr, buf.len());
            if ret < 0 {
                let msg = format!("write failed with error code {}", ret);
                return Err(DiagDeviceError::DeviceReadFailed(msg));
            }
        }
        Ok(())
    }
}

fn main() -> io::Result<()> {
    println!("Starting server");
    let listener = TcpListener::bind("0.0.0.0:43555")?;

    loop {
        println!("waiting for client...");
        let (mut client_reader, _) = listener.accept()?;
        let mut client_writer = client_reader.try_clone()?;

        println!("client connected, initializing diag device...");
        let mut dev_reader = DiagDevice::new().unwrap();
        let mut dev_writer = dev_reader.try_clone().unwrap();

        let (reader_exit_tx, reader_exit_rx) = mpsc::channel::<bool>();
        let reader_handle = thread::spawn(move || {
            loop {
                if reader_exit_rx.try_recv().is_ok() {
                    return;
                }
                match dev_reader.read_response() {
                    Ok(Some(msgs)) => {
                        println!("writing {} messages to client...", msgs.len());
                        for msg in msgs {
                            client_writer.write_all(&msg).unwrap();
                        }
                    },
                    Ok(None) => {},
                    Err(err) => {
                        println!("dev reader thread err: {}", err);
                        return;
                    },
                }
            }
        });

        let mut buf = vec![0; BUFFER_LEN];
        loop {
            let bytes_read = client_reader.read(&mut buf).unwrap();
            if bytes_read == 0 {
                println!("client disconnected, waiting for thread to exit...");
                reader_exit_tx.send(true).unwrap();
                reader_handle.join().unwrap();
                break;
            }
            println!("writing {} bytes to diag device...", bytes_read);
            dev_writer.write_request(&buf[0..bytes_read]).unwrap();
        }
    }
}

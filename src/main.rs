mod hdlc;
mod diag;
mod diag_device;
mod log_codes;

use crate::diag_device::DiagDevice;

fn main() -> diag_device::DiagResult<()> {
    // this should eventually be removed for prod
    env_logger::init();

    let file = std::fs::File::options()
        .read(true)
        .write(true)
        .open("/dev/diag")?;
    let mut dev = DiagDevice::new(&file)?;
    dev.config_logs()?;

    loop {
        for msg in dev.read_response()? {
            println!("msg: {:?}", msg);
        }
    }
}

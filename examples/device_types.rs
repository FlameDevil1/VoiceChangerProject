//! Print what Windows reports about each audio device (type, interface), for diagnostics.
use cpal::traits::{DeviceTrait, HostTrait};

fn main() {
    let host = cpal::default_host();
    for (label, devices) in [("input", host.input_devices()), ("output", host.output_devices())] {
        for d in devices.into_iter().flatten() {
            if let Ok(desc) = d.description() {
                println!("{label:6} {:?} / {:?} : {}", desc.device_type(), desc.interface_type(), desc.name());
            }
        }
    }
}

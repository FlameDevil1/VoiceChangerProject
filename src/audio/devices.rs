//! Device enumeration, lookup and virtual-cable detection.

use crate::config::DeviceRef;
use cpal::traits::{DeviceTrait, HostTrait};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
}

impl DeviceInfo {
    pub fn to_ref(&self) -> DeviceRef {
        DeviceRef { id: self.id.clone(), name: self.name.clone() }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeviceList {
    pub inputs: Vec<DeviceInfo>,
    pub outputs: Vec<DeviceInfo>,
    pub default_input: Option<String>,
    pub default_output: Option<String>,
}

impl DeviceList {
    /// Output devices that look like a virtual cable's playback side.
    pub fn cables(&self) -> impl Iterator<Item = &DeviceInfo> {
        self.outputs.iter().filter(|d| is_cable_playback(&d.name))
    }
}

fn info(d: &cpal::Device) -> Option<DeviceInfo> {
    Some(DeviceInfo { id: d.id().ok()?.to_string(), name: d.description().ok()?.name().to_string() })
}

pub fn enumerate(host: &cpal::Host) -> DeviceList {
    let collect = |it: Option<Box<dyn Iterator<Item = cpal::Device>>>| {
        let mut v: Vec<DeviceInfo> = it.into_iter().flatten().filter_map(|d| info(&d)).collect();
        v.sort_by_key(|d| d.name.to_lowercase());
        v
    };
    DeviceList {
        inputs: collect(host.input_devices().ok().map(|i| Box::new(i) as Box<dyn Iterator<Item = _>>)),
        outputs: collect(host.output_devices().ok().map(|i| Box::new(i) as Box<dyn Iterator<Item = _>>)),
        default_input: host.default_input_device().and_then(|d| info(&d)).map(|d| d.name),
        default_output: host.default_output_device().and_then(|d| info(&d)).map(|d| d.name),
    }
}

/// Resolve a remembered device: exact ID first, then by name (IDs can change when a USB device
/// moves ports), then the system default when `want` is `None`.
pub fn find(host: &cpal::Host, want: Option<&DeviceRef>, input: bool) -> Option<cpal::Device> {
    let Some(want) = want else {
        return if input { host.default_input_device() } else { host.default_output_device() };
    };
    let devices: Vec<cpal::Device> =
        if input { host.input_devices().ok()?.collect() } else { host.output_devices().ok()?.collect() };
    let by_id = devices.iter().position(|d| d.id().map(|id| id.to_string() == want.id).unwrap_or(false));
    let by_name = || devices.iter().position(|d| d.description().map(|x| x.name() == want.name).unwrap_or(false));
    by_id.or_else(by_name).map(|i| devices[i].clone())
}

pub fn name_of(d: &cpal::Device) -> String {
    d.description().map(|x| x.name().to_string()).unwrap_or_else(|_| "Unknown device".into())
}

pub fn id_of(d: &cpal::Device) -> String {
    d.id().map(|x| x.to_string()).unwrap_or_default()
}

/// The playback side of a virtual cable, which we write to.
/// VB-CABLE: "CABLE Input (VB-Audio Virtual Cable)"; also Hi-Fi Cable, CABLE-A/B, Voicemeeter.
pub fn is_cable_playback(name: &str) -> bool {
    let n = name.to_lowercase();
    (n.contains("vb-audio") || n.starts_with("cable")) && n.contains("input")
}

/// The recording side of a virtual cable ("CABLE Output"), which other apps use as a mic.
/// Selecting it as *our* input would feed the app back into itself.
pub fn is_cable_capture(name: &str) -> bool {
    let n = name.to_lowercase();
    (n.contains("vb-audio") || n.starts_with("cable")) && n.contains("output")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_vb_cable_names() {
        assert!(is_cable_playback("CABLE Input (VB-Audio Virtual Cable)"));
        assert!(is_cable_playback("CABLE-A Input (VB-Audio Cable A)"));
        assert!(is_cable_playback("Voicemeeter Input (VB-Audio Voicemeeter VAIO)"));
        assert!(!is_cable_playback("Speakers (Realtek(R) Audio)"));
        assert!(!is_cable_playback("CABLE Output (VB-Audio Virtual Cable)"));
        assert!(is_cable_capture("CABLE Output (VB-Audio Virtual Cable)"));
        assert!(!is_cable_capture("Microphone (USB Audio Device)"));
    }
}

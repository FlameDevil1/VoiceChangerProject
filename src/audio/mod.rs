//! Real-time audio engine: device enumeration, stream management and the lock-free state shared
//! between the audio callbacks, the controller thread and the UI.

pub mod devices;
pub mod engine;
pub mod shared;

pub use devices::{DeviceInfo, DeviceList};
pub use engine::{Command, EngineHandle, EngineSettings, EngineState, Status};
pub use shared::Shared;

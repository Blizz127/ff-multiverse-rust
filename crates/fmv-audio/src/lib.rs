//! STEP 0 gate stub: proves cpal and symphonia resolve and build offline. Replaced by the engine.

#[cfg(feature = "device")]
pub fn default_device_name() -> Option<String> {
    use cpal::traits::HostTrait;
    cpal::default_host().default_output_device().map(|d| d.to_string())
}

pub fn probe_decoder() -> bool {
    symphonia::default::get_probe();
    true
}

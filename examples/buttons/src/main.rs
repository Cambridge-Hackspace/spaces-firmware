use std::ffi::CStr;

fn main() {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    log::info!("spaces-firmware buttons demo {}", env!("CARGO_PKG_VERSION"));
    log::info!("running from {}", running_slot());
}

// Which of the two app slots this image booted from: the first sign that the
// partition table and the bootloader on the device are the ones built here.
fn running_slot() -> String {
    let part = unsafe { esp_idf_svc::sys::esp_ota_get_running_partition() };
    if part.is_null() {
        return "an unknown partition".to_string();
    }
    unsafe { CStr::from_ptr((*part).label.as_ptr()) }
        .to_string_lossy()
        .into_owned()
}

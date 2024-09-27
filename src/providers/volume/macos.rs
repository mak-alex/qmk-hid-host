use core_foundation::runloop::{kCFRunLoopDefaultMode, CFRunLoopRunInMode};
use core_foundation_sys::base::Boolean;
use core_foundation_sys::date::CFTimeInterval;
use coreaudio_sys::{
    AudioObjectGetPropertyData, AudioObjectPropertyAddress, kAudioObjectSystemObject,
    kAudioHardwarePropertyDefaultOutputDevice, kAudioDevicePropertyVolumeScalar,
    kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyScopeOutput, kAudioObjectPropertyElementMaster,
};
use libc::c_void;
use tokio::sync::{broadcast, mpsc};
use std::sync::{Arc, Mutex};
use crate::data_type::DataType;
use super::super::_base::Provider;


const MIN_VOLUME_CHANGE: f32 = 0.05;
const MIN_VOLUME_SEND_THRESHOLD: u8 = 1;

unsafe fn get_default_output_device() -> Option<u32> {
    let mut device_id: u32 = 0;
    let address = AudioObjectPropertyAddress {
        mSelector: kAudioHardwarePropertyDefaultOutputDevice,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMaster,
    };

    let mut size = std::mem::size_of::<u32>() as u32;
    let status = AudioObjectGetPropertyData(
        kAudioObjectSystemObject,
        &address,
        0,
        std::ptr::null(),
        &mut size,
        &mut device_id as *mut u32 as *mut c_void,
    );

    if status == 0 {
        tracing::debug!("Successfully obtained default output device: {}", device_id);
        Some(device_id)
    } else {
        tracing::error!("Failed to obtain default output device. Status: {}", status);
        None
    }
}

unsafe fn get_device_volume(device_id: u32) -> Option<f32> {
    let address = AudioObjectPropertyAddress {
        mSelector: kAudioDevicePropertyVolumeScalar,
        mScope: kAudioObjectPropertyScopeOutput,
        mElement: kAudioObjectPropertyElementMaster,
    };

    let mut volume: f32 = 0.0;
    let mut size = std::mem::size_of::<f32>() as u32;

    let status = AudioObjectGetPropertyData(
        device_id,
        &address,
        0,
        std::ptr::null(),
        &mut size,
        &mut volume as *mut f32 as *mut c_void,
    );

    if status == 0 {
        tracing::debug!("Current device volume: {}", volume);
        Some(volume)
    } else {
        tracing::error!("Failed to get device volume. Status: {}", status);
        None
    }
}


fn send_data(volume: f32, data_sender: &mpsc::Sender<Vec<u8>>) {
    let volume_percentage = (volume * 100.0).round() as u8;

    if volume_percentage > MIN_VOLUME_SEND_THRESHOLD {
        let data = vec![DataType::Volume as u8, volume_percentage];
        match data_sender.try_send(data) {
            Ok(_) => tracing::info!("Successfully sent volume data: {}%", volume_percentage),
            Err(e) => tracing::error!("Failed to send volume data: {}", e),
        }
    } else {
        tracing::debug!("Volume change {}% is too small, ignoring.", volume_percentage);
    }
}

pub struct VolumeProvider {
    data_sender: mpsc::Sender<Vec<u8>>,
    connected_sender: broadcast::Sender<bool>,
}

impl VolumeProvider {
    pub fn new(data_sender: mpsc::Sender<Vec<u8>>, connected_sender: broadcast::Sender<bool>) -> Box<dyn Provider> {
        let provider = VolumeProvider {
            data_sender,
            connected_sender,
        };
        Box::new(provider)
    }
}

impl Provider for VolumeProvider {
    fn start(&self) {
        tracing::info!("Volume Provider started");

        let data_sender = self.data_sender.clone();
        let connected_sender = self.connected_sender.clone();
        let mut synced_volume = 0.0;

        let is_connected = Arc::new(Mutex::new(true));
        let is_connected_ref = is_connected.clone();
        std::thread::spawn(move || {
            let mut connected_receiver = connected_sender.subscribe();
            loop {
                if !connected_receiver.try_recv().unwrap_or(true) {
                    let mut is_connected = is_connected_ref.lock().unwrap();
                    *is_connected = false;
                    break;
                }

                std::thread::sleep(std::time::Duration::from_millis(100)); // Увеличено до 1000 мс
            }
        });

        loop {
            if !*(is_connected.lock().unwrap()) {
                break;
            }

            unsafe {
                if let Some(device_id) = get_default_output_device() {
                    if let Some(volume) = get_device_volume(device_id) {
                        let volume_change = (volume - synced_volume).abs();
                        if volume_change > MIN_VOLUME_CHANGE {
                            tracing::debug!(
                                "Volume changed from {} to {}, change: {}",
                                synced_volume,
                                volume,
                                volume_change
                            );
                            synced_volume = volume;
                            send_data(volume, &data_sender);
                        } else {
                            tracing::debug!(
                                "Volume change too small: {} (threshold: {})",
                                volume_change,
                                MIN_VOLUME_CHANGE
                            );
                        }
                    } else {
                        tracing::warn!("Failed to obtain volume for device ID: {}", device_id);
                    }
                } else {
                    tracing::warn!("No default output device found.");
                }
            }

            unsafe {
                CFRunLoopRunInMode(kCFRunLoopDefaultMode, CFTimeInterval::from(1.0), Boolean::from(true));
            }
        }

        tracing::info!("Volume Provider stopped");
    }
}

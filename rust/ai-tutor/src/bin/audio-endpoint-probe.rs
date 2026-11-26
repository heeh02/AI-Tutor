//! Read-only Windows microphone endpoint diagnosis. No capture or settings writes.
use anyhow::Result;
use serde_json::{Value, json};
use std::path::PathBuf;
use windows::{
    Win32::{
        Devices::FunctionDiscovery::PKEY_Device_FriendlyName,
        Media::Audio::{
            DEVICE_STATE_ACTIVE, Endpoints::IAudioEndpointVolume, IMMDevice, IMMDeviceEnumerator,
            MMDeviceEnumerator, eCapture, eCommunications, eConsole, eMultimedia,
        },
        System::Com::{
            CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
            CoUninitialize, STGM_READ,
            StructuredStorage::{PropVariantClear, PropVariantToStringAlloc},
        },
    },
    core::PWSTR,
};

struct ComScope;
impl Drop for ComScope {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}
unsafe fn owned_string(value: PWSTR) -> Result<String> {
    let text = unsafe { value.to_string() };
    unsafe { CoTaskMemFree(Some(value.0.cast())) };
    Ok(text?)
}
unsafe fn device_id(device: &IMMDevice) -> Result<String> {
    unsafe { owned_string(device.GetId()?) }
}
unsafe fn friendly_name(device: &IMMDevice) -> Result<String> {
    unsafe {
        let store = device.OpenPropertyStore(STGM_READ)?;
        let mut variant = store.GetValue(&PKEY_Device_FriendlyName)?;
        let text = PropVariantToStringAlloc(&variant);
        let cleared = PropVariantClear(&mut variant);
        let text = text?;
        let result = owned_string(text);
        cleared?;
        result
    }
}

fn snapshot() -> Result<Value> {
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        let _scope = ComScope;
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let default_console = enumerator
            .GetDefaultAudioEndpoint(eCapture, eConsole)
            .ok()
            .and_then(|d| device_id(&d).ok());
        let default_communications = enumerator
            .GetDefaultAudioEndpoint(eCapture, eCommunications)
            .ok()
            .and_then(|d| device_id(&d).ok());
        let default_multimedia = enumerator
            .GetDefaultAudioEndpoint(eCapture, eMultimedia)
            .ok()
            .and_then(|d| device_id(&d).ok());
        let devices = enumerator.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE)?;
        let count = devices.GetCount()?;
        let mut results = Vec::new();
        for index in 0..count {
            let device = devices.Item(index)?;
            let id = device_id(&device)?;
            let name = friendly_name(&device).unwrap_or_else(|e| format!("Name unavailable: {e}"));
            let mut item = json!({"stable_endpoint_id":id,"friendly_name":name,"obsbot_match":name.to_uppercase().contains("OBSBOT"),"default_console":default_console.as_ref()==Some(&id),"default_communications":default_communications.as_ref()==Some(&id),"default_multimedia":default_multimedia.as_ref()==Some(&id),"device_state":device.GetState()?.0});
            match device.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None) {
                Ok(volume) => {
                    item["endpoint_volume_interface"] = json!("available");
                    item["muted"] = match volume.GetMute() {
                        Ok(v) => json!(v.as_bool()),
                        Err(e) => json!({"error":e.to_string()}),
                    };
                    item["master_volume_scalar"] = match volume.GetMasterVolumeLevelScalar() {
                        Ok(v) => json!(v),
                        Err(e) => json!({"error":e.to_string()}),
                    };
                    item["master_volume_db"] = match volume.GetMasterVolumeLevel() {
                        Ok(v) => json!(v),
                        Err(e) => json!({"error":e.to_string()}),
                    };
                    item["hardware_support_flags"] = match volume.QueryHardwareSupport() {
                        Ok(v) => json!(v),
                        Err(e) => json!({"error":e.to_string()}),
                    };
                    let channels = volume.GetChannelCount()?;
                    let mut levels = Vec::new();
                    for channel in 0..channels.min(32) {
                        levels.push(match volume.GetChannelVolumeLevelScalar(channel) {
                            Ok(value) => json!({"channel":channel,"scalar":value}),
                            Err(e) => json!({"channel":channel,"error":e.to_string()}),
                        });
                    }
                    item["channels"] = json!(levels);
                }
                Err(error) => {
                    item["endpoint_volume_interface"] = json!({"error":error.to_string()})
                }
            }
            results.push(item);
        }
        Ok(
            json!({"at":chrono::Utc::now().with_timezone(&chrono::FixedOffset::east_opt(28800).unwrap()).to_rfc3339(),"test_type":"read_only_native_audio_endpoint_query","settings_changed":false,"capture_started":false,"audio_played":false,"default_console_endpoint":default_console,"default_communications_endpoint":default_communications,"default_multimedia_endpoint":default_multimedia,"active_capture_endpoints":results,"scope_note":"Windows endpoint mute and gain only; does not prove microphone hardware/vendor DSP state or speech audibility."}),
        )
    }
}

fn main() -> Result<()> {
    let root = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or(std::env::current_dir()?);
    let result = snapshot()?;
    let output = root.join("runtime/rust_audio_endpoint_probe.json");
    std::fs::create_dir_all(output.parent().unwrap())?;
    std::fs::write(output, serde_json::to_vec_pretty(&result)?)?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

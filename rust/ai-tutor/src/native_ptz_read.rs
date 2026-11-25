//! Read-only DirectShow controller query. No Set call, graph, stream or device command.
use serde_json::{Value, json};
use std::{
    fs,
    path::Path,
    thread,
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Media::DirectShow::{
            CameraControl_Pan, CameraControl_Tilt, IAMCameraControl, IBaseFilter, ICreateDevEnum,
        },
        System::{
            Com::{
                CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
                CoUninitialize, IBindCtx, IEnumMoniker, IErrorLog, IMoniker,
                StructuredStorage::IPropertyBag,
            },
            Variant::{VARIANT, VT_BSTR, VariantClear},
        },
    },
    core::{GUID, Interface, PCWSTR, w},
};

// Windows SDK CLSID_SystemDeviceEnum / CLSID_VideoInputDeviceCategory.
const SYSTEM_DEVICE_ENUM: GUID = GUID::from_u128(0x62be5d10_60eb_11d0_bd3b_00a0c911ce86);
const VIDEO_INPUT_CATEGORY: GUID = GUID::from_u128(0x860bb310_5d01_11d0_bd3b_00a0c911ce86);

pub fn device_paths_match(expected: &str, actual: &str) -> bool {
    !expected.is_empty()
        && !expected.contains('\0')
        && !actual.contains('\0')
        && expected.eq_ignore_ascii_case(actual)
}

pub fn existing_device_path(root: &Path) -> Result<String, String> {
    if let Ok(path) = fs::read_to_string(root.join("runtime/camera_device.txt")) {
        if !path.is_empty() && !path.contains('\0') && !path.contains('\n') && !path.contains('\r')
        {
            return Ok(path);
        }
    }
    let bytes = fs::read(root.join("runtime/uvc_probe.json"))
        .map_err(|e| format!("Missing exact historical DirectShow device path: {e}"))?;
    let report: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    report["devices"]
        .as_array()
        .and_then(|ds| ds.iter().find(|d| d["matches_saved_stable_path"] == true))
        .and_then(|d| d["device_path"].as_str())
        .filter(|p| !p.is_empty() && !p.contains('\0'))
        .map(str::to_owned)
        .ok_or("No exact DirectShow device path is available".into())
}

struct ComApartment;
impl ComApartment {
    fn new() -> Result<Self, String> {
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
            .ok()
            .map_err(|e| format!("CoInitializeEx: {e}"))?;
        Ok(Self)
    }
}
impl Drop for ComApartment {
    fn drop(&mut self) {
        unsafe { CoUninitialize() }
    }
}
struct OwnedVariant(VARIANT);
impl Default for OwnedVariant {
    fn default() -> Self {
        Self(VARIANT::default())
    }
}
impl Drop for OwnedVariant {
    fn drop(&mut self) {
        let _ = unsafe { VariantClear(&mut self.0) };
    }
}

fn read_string(bag: &IPropertyBag, key: PCWSTR) -> Result<String, String> {
    let mut value = OwnedVariant::default();
    unsafe { bag.Read(key, &mut value.0, None::<&IErrorLog>) }.map_err(|e| e.to_string())?;
    let vt = unsafe { value.0.Anonymous.Anonymous.vt };
    if vt != VT_BSTR {
        return Err("Device property is not a BSTR".into());
    }
    Ok(unsafe { value.0.Anonymous.Anonymous.Anonymous.bstrVal.to_string() })
}
fn hresult(error: &windows::core::Error) -> String {
    format!("0x{:08X}", error.code().0 as u32)
}

fn range(control: &IAMCameraControl, property: i32) -> Value {
    let (mut minimum, mut maximum, mut step, mut default, mut flags) = (0, 0, 0, 0, 0);
    match unsafe {
        control.GetRange(
            property,
            &mut minimum,
            &mut maximum,
            &mut step,
            &mut default,
            &mut flags,
        )
    } {
        Ok(()) => {
            json!({"get_range_hresult":"0x00000000","range_supported":true,"minimum":minimum,"maximum":maximum,"step":step,"default":default,"capability_flags":flags,"manual_supported":flags&2!=0,"auto_property_supported":flags&1!=0,"relative_flag":flags&16!=0,"units":"absolute pan/tilt degrees per Microsoft documentation; physical direction not calibrated"})
        }
        Err(e) => {
            json!({"get_range_hresult":hresult(&e),"range_supported":false,"error":e.to_string()})
        }
    }
}
fn current(control: &IAMCameraControl, property: i32) -> Value {
    let (mut value, mut flags) = (0, 0);
    match unsafe { control.Get(property, &mut value, &mut flags) } {
        Ok(()) => json!({"get_hresult":"0x00000000","value":value,"flags":flags}),
        Err(e) => {
            json!({"get_hresult":hresult(&e),"value":null,"flags":null,"error":e.to_string()})
        }
    }
}
fn stability(samples: &[Value]) -> Value {
    let pan: Vec<_> = samples
        .iter()
        .filter_map(|s| s["pan"]["value"].as_i64())
        .collect();
    let tilt: Vec<_> = samples
        .iter()
        .filter_map(|s| s["tilt"]["value"].as_i64())
        .collect();
    let complete = pan.len() == samples.len() && tilt.len() == samples.len() && !samples.is_empty();
    json!({"all_current_reads_succeeded":complete,"pan_min":pan.iter().min(),"pan_max":pan.iter().max(),"tilt_min":tilt.iter().min(),"tilt_max":tilt.iter().max(),
        "position_stable_during_window":if complete&&samples.len()>1{json!(pan.iter().all(|v|*v==pan[0])&&tilt.iter().all(|v|*v==tilt[0]))}else{Value::Null},
        "interpretation":"Stable standard-property readback does not prove vendor AI tracking is disabled; changing readback is an observation, not an identified cause."})
}

/// No filter graph or stream is created. Only the exact matched moniker is bound.
pub fn read_only_probe(
    expected: &str,
    sample_count: usize,
    interval: Duration,
) -> Result<Value, String> {
    if !device_paths_match(expected, expected) {
        return Err("Invalid exact device path".into());
    }
    if !(1..=20).contains(&sample_count)
        || interval > Duration::from_secs(1)
        || (sample_count == 1 && !interval.is_zero())
    {
        return Err("Read-only sample window exceeds bounds".into());
    }
    let _apartment = ComApartment::new()?;
    let device_enum: ICreateDevEnum =
        unsafe { CoCreateInstance(&SYSTEM_DEVICE_ENUM, None, CLSCTX_INPROC_SERVER) }
            .map_err(|e| e.to_string())?;
    let mut monikers: Option<IEnumMoniker> = None;
    unsafe { device_enum.CreateClassEnumerator(&VIDEO_INPUT_CATEGORY, &mut monikers, 0) }
        .map_err(|e| e.to_string())?;
    let enumerator = monikers.ok_or("No native video monikers were enumerated")?;
    let mut devices = Vec::new();
    for _ in 0..32 {
        let mut one: [Option<IMoniker>; 1] = [None];
        let mut fetched = 0;
        let hr = unsafe { enumerator.Next(&mut one, Some(&mut fetched)) };
        if hr.0 == 1 {
            break;
        }
        hr.ok().map_err(|e| e.to_string())?;
        if fetched == 0 {
            break;
        }
        let moniker = one[0]
            .take()
            .ok_or("Enumerator returned an empty moniker")?;
        let bag: IPropertyBag =
            unsafe { moniker.BindToStorage(None::<&IBindCtx>, None::<&IMoniker>) }
                .map_err(|e| e.to_string())?;
        let name =
            read_string(&bag, w!("FriendlyName")).unwrap_or_else(|_| "Unnamed video device".into());
        let path = read_string(&bag, w!("DevicePath"))?;
        let matches = device_paths_match(expected, &path);
        devices.push(json!({"name":name,"device_path":path,"matches_exact_saved_path":matches}));
        if !matches {
            continue;
        }
        let filter: IBaseFilter =
            unsafe { moniker.BindToObject(None::<&IBindCtx>, None::<&IMoniker>) }
                .map_err(|e| format!("Bind camera controller filter: {e}"))?;
        let control: IAMCameraControl = filter
            .cast()
            .map_err(|e| format!("Query IAMCameraControl: {e}"))?;
        let ranges = json!({"pan":range(&control,CameraControl_Pan.0),"tilt":range(&control,CameraControl_Tilt.0)});
        let start = Instant::now();
        let mut samples = Vec::with_capacity(sample_count);
        for i in 0..sample_count {
            samples.push(json!({"sample":i,"elapsed_ms":start.elapsed().as_millis(),"pan":current(&control,CameraControl_Pan.0),"tilt":current(&control,CameraControl_Tilt.0)}));
            if i + 1 < sample_count {
                thread::sleep(interval)
            }
        }
        let stable = stability(&samples);
        let pass = ranges["pan"]["range_supported"] == true
            && ranges["tilt"]["range_supported"] == true
            && stable["all_current_reads_succeeded"] == true;
        return Ok(
            json!({"schema_version":"1.0","implementation":"Rust native Windows DirectShow typed bindings","timestamp_utc":chrono::Utc::now().to_rfc3339(),"result":if pass{"PASS_READ_ONLY_QUERY"}else{"FAIL"},
            "expected_device_path":expected,"matched_device":{"name":name,"device_path":path},"enumerated_devices":devices,"exact_device_path_match":true,
            "read_only":true,"set_calls":0,"capture_graph_created":false,"capture_stream_started":false,"ptz_motion_commands":0,
            "ranges":ranges,"samples":samples,"stability":stable,"sample_interval_ms":interval.as_millis(),"sample_count":sample_count,
            "vendor_auto_state":"unknown","vendor_tracking_off_verified":false,"vendor_status_caveat":"CameraControl Auto/Manual flags refer to standard properties, not OBSBOT person/gesture tracking. This probe does not toggle tracking."}),
        );
    }
    Ok(
        json!({"result":"DEVICE_NOT_MATCHED","read_only":true,"set_calls":0,"capture_graph_created":false,"capture_stream_started":false,"expected_device_path":expected,"enumerated_devices":devices,"exact_device_path_match":false,"vendor_auto_state":"unknown"}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_device_path_accepts_case_only_changes() {
        let path = r"\\?\usb#vid_3564&pid_ff02&mi_00#6&736750&0&0000#{65e8773d-8f56-11d0-a3b9-00a0c9223196}\global";
        assert!(device_paths_match(path, &path.to_ascii_uppercase()));
        assert!(!device_paths_match(
            path,
            &path.replace("736750", "DIFFERENT")
        ));
        assert!(!device_paths_match(
            path,
            &path.replace(
                "65e8773d-8f56-11d0-a3b9-00a0c9223196",
                "e5323777-f976-4f5b-9b55-b94699c46e44"
            )
        ));
        assert!(!device_paths_match(path, &path[..path.len() - 7]));
    }
    #[test]
    fn rejects_empty_nul_and_prefix_matches() {
        assert!(!device_paths_match("", ""));
        assert!(!device_paths_match("abc\0", "abc\0"));
        assert!(!device_paths_match("abc", "abcdef"));
    }
    #[test]
    fn stability_never_upgrades_to_vendor_tracking_off() {
        let samples = vec![
            json!({"pan":{"value":7},"tilt":{"value":1}}),
            json!({"pan":{"value":7},"tilt":{"value":1}}),
        ];
        assert_eq!(stability(&samples)["position_stable_during_window"], true);
        let drift = vec![
            samples[0].clone(),
            json!({"pan":{"value":7},"tilt":{"value":2}}),
        ];
        assert_eq!(stability(&drift)["position_stable_during_window"], false);
        assert_eq!(
            stability(&samples[..1])["position_stable_during_window"],
            Value::Null
        );
        assert_eq!(
            stability(&samples[..1])["all_current_reads_succeeded"],
            true
        );
    }
}

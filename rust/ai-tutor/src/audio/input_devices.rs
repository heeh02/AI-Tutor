//! Process-local input selection. Only the capture owner enumerates devices.
//! A key retains the original CPAL handle; names and list positions are labels.
use anyhow::{Result, ensure};
use cpal::traits::{DeviceTrait, HostTrait};
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

pub(crate) const CHANGED: &str = "麦克风设备已变化，请刷新设备列表并重新选择";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InputBinding {
    pub device_key: String,
    pub topology_revision: u64,
}
#[derive(Clone, Debug)]
pub(crate) enum InputSelection {
    Legacy(Option<usize>),
    Bound(InputBinding),
}
impl InputSelection {
    pub(crate) fn binding(&self) -> Option<&InputBinding> {
        match self {
            Self::Bound(binding) => Some(binding),
            Self::Legacy(_) => None,
        }
    }
}

pub(super) struct Topology {
    revision: AtomicU64,
    available: AtomicBool,
}
impl Topology {
    fn new() -> Self {
        Self {
            revision: AtomicU64::new(1),
            available: AtomicBool::new(false),
        }
    }
    pub(super) fn changed(&self) {
        self.revision.fetch_add(1, Ordering::SeqCst);
    }
    fn revision(&self) -> u64 {
        self.revision.load(Ordering::SeqCst)
    }
    fn available(&self) -> bool {
        self.available.load(Ordering::SeqCst)
    }
    fn matches(&self, binding: &InputBinding) -> bool {
        self.available() && binding.topology_revision == self.revision()
    }
}
struct Entry<D> {
    key: String,
    device: D,
}
// The small generic table lets tests use fake handles without initializing CPAL.
struct Registry<D> {
    entries: Vec<Entry<D>>,
}
impl<D> Registry<D> {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }
    fn refresh(&mut self, devices: Vec<D>, same: impl Fn(&D, &D) -> bool) -> bool {
        let mut old = std::mem::take(&mut self.entries);
        let mut changed = false;
        for device in devices {
            if let Some(index) = old.iter().position(|entry| same(&entry.device, &device)) {
                // Preserve the actual original handle, including across reordering.
                self.entries.push(old.remove(index));
            } else {
                changed = true;
                self.entries.push(Entry {
                    key: uuid::Uuid::new_v4().to_string(),
                    device,
                });
            }
        }
        changed || !old.is_empty()
    }
    fn find(&self, key: &str) -> Option<(usize, &D)> {
        self.entries
            .iter()
            .enumerate()
            .find(|(_, entry)| entry.key == key)
            .map(|(index, entry)| (index, &entry.device))
    }
}

pub(super) struct InputDevices {
    pub(super) topology: Arc<Topology>,
    registry: Mutex<Registry<cpal::Device>>,
    #[cfg(test)]
    fake_binding: Mutex<Option<InputBinding>>,
    #[cfg(all(test, windows))]
    voice_loop_source: Mutex<Option<Arc<super::voice_loop_source::WavSource>>>,
}
impl InputDevices {
    pub(super) fn new() -> Self {
        Self {
            topology: Arc::new(Topology::new()),
            registry: Mutex::new(Registry::new()),
            #[cfg(test)]
            fake_binding: Mutex::new(None),
            #[cfg(all(test, windows))]
            voice_loop_source: Mutex::new(None),
        }
    }
    pub(super) fn stamp_status(&self, status: &mut Value) {
        status["input_topology_revision"] = json!(self.topology.revision());
        status["stable_binding_available"] = json!(self.topology.available());
    }
    pub(super) fn binding_current(&self, binding: Option<&InputBinding>) -> bool {
        binding.is_none_or(|binding| self.topology.matches(binding))
    }
    pub(super) fn validate(&self, selection: &InputSelection) -> Result<()> {
        if let Some(binding) = selection.binding() {
            #[cfg(test)]
            if let Some(expected) = self.fake_binding.lock().unwrap().as_ref() {
                ensure!(
                    binding == expected && self.topology.matches(binding),
                    CHANGED
                );
                return Ok(());
            }
            self.resolve(binding)?;
        }
        Ok(())
    }
    pub(super) fn resolve(&self, binding: &InputBinding) -> Result<(usize, cpal::Device)> {
        let registry = self.registry.lock().unwrap();
        ensure!(self.topology.matches(binding), CHANGED);
        registry
            .find(&binding.device_key)
            .map(|(index, device)| (index, device.clone()))
            .ok_or_else(|| anyhow::anyhow!(CHANGED))
    }
    /// Owner-only: querying metadata does not create an input stream.
    pub(super) fn enumerate(&self) -> Result<Value> {
        let before = self.topology.revision();
        let devices = cpal::default_host().input_devices()?.collect();
        let mut registry = self.registry.lock().unwrap();
        ensure!(before == self.topology.revision(), CHANGED);
        if registry.refresh(devices, same_device) {
            self.topology.changed();
        }
        let revision = self.topology.revision();
        let stable = self.topology.available();
        let list = registry
            .entries
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                let config = entry.device.default_input_config().ok();
                json!({"device_id":index,"device_key":if stable {Some(&entry.key)} else {None},
                "input_topology_revision":revision,"stable_binding_available":stable,
                "name":entry.device.name().unwrap_or_else(|_| "Input device".into()),
                "sample_rate":config.as_ref().map(|c| c.sample_rate().0),
                "input_channels":config.map(|c| c.channels()),"hostapi":"WASAPI"})
            })
            .collect::<Vec<_>>();
        ensure!(revision == self.topology.revision(), CHANGED);
        Ok(json!(list))
    }
    pub(super) fn clear(&self) {
        self.topology.available.store(false, Ordering::SeqCst);
        self.topology.changed();
        self.registry.lock().unwrap().entries.clear();
    }
    #[cfg(test)]
    pub(super) fn test_binding(&self) -> InputBinding {
        self.topology.available.store(true, Ordering::SeqCst);
        InputBinding {
            device_key: "fake-no-device".into(),
            topology_revision: self.topology.revision(),
        }
    }
    #[cfg(test)]
    pub(super) fn install_fake_binding(&self) -> InputBinding {
        self.topology.available.store(true, Ordering::SeqCst);
        let binding = InputBinding {
            device_key: uuid::Uuid::new_v4().to_string(),
            topology_revision: self.topology.revision(),
        };
        *self.fake_binding.lock().unwrap() = Some(binding.clone());
        binding
    }
    #[cfg(all(test, windows))]
    pub(super) fn install_voice_loop_source(
        &self,
        source: Arc<super::voice_loop_source::WavSource>,
    ) {
        *self.voice_loop_source.lock().unwrap() = Some(source);
    }
    #[cfg(all(test, windows))]
    pub(super) fn voice_loop_source(
        &self,
        selection: &InputSelection,
    ) -> Result<Option<Arc<super::voice_loop_source::WavSource>>> {
        let source = self.voice_loop_source.lock().unwrap().clone();
        if let Some(source) = &source {
            // An installed diagnostic source never falls through to a physical
            // device on a wrong binding, including a legacy-index request.
            ensure!(source.matches(selection), CHANGED);
        }
        Ok(source)
    }
}

#[cfg(target_os = "windows")]
fn same_device(left: &cpal::Device, right: &cpal::Device) -> bool {
    // CPAL 0.16's public WASAPI PartialEq compares opaque IMMDevice::GetId.
    // No layout casts, private API, display-name equality or index fallback.
    match (left.as_inner(), right.as_inner()) {
        (cpal::platform::DeviceInner::Wasapi(left), cpal::platform::DeviceInner::Wasapi(right)) => {
            left == right
        }
        #[allow(unreachable_patterns)]
        _ => false,
    }
}
#[cfg(not(target_os = "windows"))]
fn same_device(_: &cpal::Device, _: &cpal::Device) -> bool {
    false
}

#[cfg(target_os = "windows")]
mod notifications {
    use super::*;
    use windows::{
        Win32::{
            Foundation::PROPERTYKEY,
            Media::Audio::{
                DEVICE_STATE, EDataFlow, ERole, IMMDeviceEnumerator, IMMNotificationClient,
                IMMNotificationClient_Impl, MMDeviceEnumerator, eAll, eCapture,
            },
            System::Com::{
                CLSCTX_ALL, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
                CoUninitialize,
            },
        },
        core::{PCWSTR, implement},
    };

    #[implement(IMMNotificationClient)]
    struct Notifications {
        topology: Arc<Topology>,
    }
    #[allow(non_snake_case)]
    impl IMMNotificationClient_Impl for Notifications_Impl {
        fn OnDeviceStateChanged(&self, _: &PCWSTR, _: DEVICE_STATE) -> windows::core::Result<()> {
            self.topology.changed();
            Ok(())
        }
        fn OnDeviceAdded(&self, _: &PCWSTR) -> windows::core::Result<()> {
            self.topology.changed();
            Ok(())
        }
        fn OnDeviceRemoved(&self, _: &PCWSTR) -> windows::core::Result<()> {
            self.topology.changed();
            Ok(())
        }
        fn OnDefaultDeviceChanged(
            &self,
            flow: EDataFlow,
            _: ERole,
            _: &PCWSTR,
        ) -> windows::core::Result<()> {
            if flow == eCapture || flow == eAll {
                self.topology.changed();
            }
            Ok(())
        }
        fn OnPropertyValueChanged(&self, _: &PCWSTR, _: &PROPERTYKEY) -> windows::core::Result<()> {
            self.topology.changed();
            Ok(())
        }
    }
    // Declared last in the monitor so COM outlives both interface references.
    struct Apartment;
    impl Drop for Apartment {
        fn drop(&mut self) {
            unsafe {
                CoUninitialize();
            }
        }
    }
    pub(in crate::audio) struct Monitor {
        enumerator: IMMDeviceEnumerator,
        callback: IMMNotificationClient,
        topology: Arc<Topology>,
        _apartment: Apartment,
    }
    impl Monitor {
        pub(in crate::audio) fn new(topology: Arc<Topology>) -> Result<Self> {
            unsafe {
                CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
            }
            let apartment = Apartment;
            let enumerator: IMMDeviceEnumerator =
                unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)? };
            let callback: IMMNotificationClient = Notifications {
                topology: topology.clone(),
            }
            .into();
            unsafe {
                enumerator.RegisterEndpointNotificationCallback(&callback)?;
            }
            topology.available.store(true, Ordering::SeqCst);
            Ok(Self {
                enumerator,
                callback,
                topology,
                _apartment: apartment,
            })
        }
    }
    impl Drop for Monitor {
        fn drop(&mut self) {
            self.topology.available.store(false, Ordering::SeqCst);
            self.topology.changed();
            unsafe {
                let _ = self
                    .enumerator
                    .UnregisterEndpointNotificationCallback(&self.callback);
            }
        }
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        use windows::Win32::Media::Audio::{eConsole, eRender};
        #[test]
        fn pure_output_default_notification_does_not_invalidate_input_binding() {
            let topology = Arc::new(Topology::new());
            // This constructs our callback object only; no COM service,
            // enumerator, microphone or notification registration is used.
            let callback: IMMNotificationClient = Notifications {
                topology: topology.clone(),
            }
            .into();
            let before = topology.revision();
            unsafe {
                callback
                    .OnDefaultDeviceChanged(eRender, eConsole, PCWSTR::null())
                    .unwrap();
            }
            assert_eq!(topology.revision(), before);
            unsafe {
                callback
                    .OnDefaultDeviceChanged(eCapture, eConsole, PCWSTR::null())
                    .unwrap();
            }
            assert_eq!(topology.revision(), before + 1);
        }
    }
}
#[cfg(target_os = "windows")]
pub(super) use notifications::Monitor;
#[cfg(not(target_os = "windows"))]
pub(super) struct Monitor;
#[cfg(not(target_os = "windows"))]
impl Monitor {
    pub(super) fn new(_: Arc<Topology>) -> Result<Self> {
        anyhow::bail!("稳定设备绑定仅支持 WASAPI");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Clone)]
    struct Fake {
        id: u8,
        handle: Arc<u8>,
        name: &'static str,
    }
    fn fake(id: u8) -> Fake {
        Fake {
            id,
            handle: Arc::new(id),
            name: "same name",
        }
    }
    fn same(a: &Fake, b: &Fake) -> bool {
        a.id == b.id
    }
    #[test]
    fn retained_device_key_survives_reorder_and_never_matches_by_name() {
        let first = fake(1);
        let original = first.handle.clone();
        let mut registry = Registry::new();
        assert!(registry.refresh(vec![first, fake(2)], same));
        let key = registry.entries[0].key.clone();
        assert_eq!(
            registry.entries[0].device.name,
            registry.entries[1].device.name
        );
        assert_ne!(key, registry.entries[1].key);
        assert!(!registry.refresh(vec![fake(2), fake(1)], same));
        let (index, found) = registry.find(&key).unwrap();
        assert_eq!(index, 1);
        assert!(Arc::ptr_eq(&found.handle, &original));
    }
    #[test]
    fn removed_and_readded_device_never_revives_an_old_key() {
        let mut registry = Registry::new();
        registry.refresh(vec![fake(1)], same);
        let old = registry.entries[0].key.clone();
        assert!(registry.refresh(vec![], same));
        assert!(registry.refresh(vec![fake(1)], same));
        assert!(registry.find(&old).is_none());
        assert_ne!(registry.entries[0].key, old);
        let mut next_process = Registry::new();
        next_process.refresh(vec![fake(1)], same);
        assert!(next_process.find(&registry.entries[0].key).is_none());
    }
    #[test]
    fn topology_change_invalidates_even_a_quick_unplug_replug_between_lists() {
        let topology = Topology::new();
        topology.available.store(true, Ordering::SeqCst);
        let binding = InputBinding {
            device_key: "opaque".into(),
            topology_revision: topology.revision(),
        };
        assert!(topology.matches(&binding));
        topology.changed();
        topology.changed();
        assert!(!topology.matches(&binding));
        let renewed = InputBinding {
            topology_revision: topology.revision(),
            ..binding
        };
        assert!(topology.matches(&renewed));
        topology.available.store(false, Ordering::SeqCst);
        assert!(!topology.matches(&renewed));
    }
    #[test]
    fn status_and_unknown_binding_do_not_initialize_or_enumerate_devices() {
        let devices = InputDevices::new();
        let mut status = json!({});
        devices.stamp_status(&mut status);
        assert_eq!(status["stable_binding_available"], false);
        assert_eq!(status["input_topology_revision"], 1);
        assert!(devices.validate(&InputSelection::Legacy(None)).is_ok());
        assert!(
            devices
                .validate(&InputSelection::Bound(InputBinding {
                    device_key: "foreign".into(),
                    topology_revision: 1
                }))
                .is_err()
        );
    }
}

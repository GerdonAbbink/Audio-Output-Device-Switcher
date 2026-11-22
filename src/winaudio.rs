//! Windows Core Audio: list playback devices, read their form factor, and
//! read/set the default playback device (fallback when Sonar isn't running).
//!
//! Sonar's device ids are the same MMDevice endpoint ids, so the form-factor
//! lookup is used for Sonar devices too.

use std::ffi::c_void;
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::UI::Shell::PropertiesSystem::PROPERTYKEY;
use windows::Win32::Media::Audio::{
    DEVICE_STATE_ACTIVE, ERole, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator, eCommunications, eConsole,
    eMultimedia, eRender,
};
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree, STGM_READ,
};
use windows::core::{GUID, HRESULT, HSTRING, IUnknown, Interface, PCWSTR};

use crate::model::Kind;

const PKEY_AUDIO_ENDPOINT_FORM_FACTOR: PROPERTYKEY = PROPERTYKEY {
    fmtid: GUID::from_u128(0x1da5d803_d492_4edd_8c23_e0c0ffee7f0e),
    pid: 0,
};

// Undocumented IPolicyConfig (Windows 7+), used by every "switch default
// audio device" tool. Only SetDefaultEndpoint (vtable slot 13) is called.
const CLSID_POLICY_CONFIG_CLIENT: GUID = GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9);
const IID_IPOLICY_CONFIG: GUID = GUID::from_u128(0xf8679f50_850a_41cf_9c72_430f290290c8);

pub struct WinAudio {
    enumerator: IMMDeviceEnumerator,
}

fn device_id(dev: &IMMDevice) -> windows::core::Result<String> {
    unsafe {
        let p = dev.GetId()?;
        let s = p.to_string().unwrap_or_default();
        CoTaskMemFree(Some(p.0 as *const c_void));
        Ok(s)
    }
}

impl WinAudio {
    /// Must be called on the thread that will use it (initialises COM MTA).
    pub fn new() -> windows::core::Result<Self> {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
            Ok(WinAudio { enumerator })
        }
    }

    /// Active playback devices as `(id, friendly name)`.
    pub fn output_devices(&self) -> windows::core::Result<Vec<(String, String)>> {
        let mut out = Vec::new();
        unsafe {
            let col = self.enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?;
            for i in 0..col.GetCount()? {
                let dev = col.Item(i)?;
                let id = device_id(&dev)?;
                let store = dev.OpenPropertyStore(STGM_READ)?;
                let name = store
                    .GetValue(&PKEY_Device_FriendlyName)
                    .map(|v| v.to_string())
                    .unwrap_or_else(|_| "Unknown device".into());
                out.push((id, name));
            }
        }
        Ok(out)
    }

    pub fn default_output(&self) -> windows::core::Result<String> {
        unsafe { device_id(&self.enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?) }
    }

    /// Headphones / headset / handset form factors count as headset.
    pub fn form_factor_kind(&self, id: &str) -> Option<Kind> {
        unsafe {
            let dev = self.enumerator.GetDevice(&HSTRING::from(id)).ok()?;
            let store = dev.OpenPropertyStore(STGM_READ).ok()?;
            let v = store.GetValue(&PKEY_AUDIO_ENDPOINT_FORM_FACTOR).ok()?;
            let ff = u32::try_from(&v).ok()?;
            Some(match ff {
                3 | 5 | 6 => Kind::Headset, // Headphones, Headset, Handset
                _ => Kind::Speakers,
            })
        }
    }

    /// Make `id` the default playback device for all roles.
    pub fn set_default_output(&self, id: &str) -> windows::core::Result<()> {
        unsafe {
            let unk: IUnknown = CoCreateInstance(&CLSID_POLICY_CONFIG_CLIENT, None, CLSCTX_ALL)?;
            let mut raw: *mut c_void = std::ptr::null_mut();
            unk.query(&IID_IPOLICY_CONFIG, &mut raw).ok()?;
            let policy = IUnknown::from_raw(raw); // released on drop
            type SetDefaultEndpoint = unsafe extern "system" fn(*mut c_void, PCWSTR, ERole) -> HRESULT;
            let vtbl = *(raw as *const *const usize);
            let set: SetDefaultEndpoint = std::mem::transmute(*vtbl.add(13));
            let wide = HSTRING::from(id);
            for role in [eConsole, eMultimedia, eCommunications] {
                set(raw, PCWSTR(wide.as_ptr()), role).ok()?;
            }
            drop(policy);
            Ok(())
        }
    }
}

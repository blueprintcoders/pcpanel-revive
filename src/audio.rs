//! Windows Core Audio: device volume/mute, per-app session volume/mute, default device switching.
//! All calls must happen on a thread that called `com_init`.
#![allow(non_snake_case)] // COM method names
use windows::core::{interface, Interface, GUID, HRESULT, IUnknown, IUnknown_Vtbl, PCWSTR, PWSTR};
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::{CloseHandle, S_OK};
use windows::Win32::Media::Audio::Endpoints::{IAudioEndpointVolume, IAudioMeterInformation};
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::Threading::*;
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

pub fn com_init() {
    unsafe { let _ = CoInitializeEx(None, COINIT_MULTITHREADED); }
}

#[interface("f8679f50-850a-41cf-9c72-430f290290c8")]
unsafe trait IPolicyConfig: IUnknown {
    fn _0(&self) -> HRESULT; fn _1(&self) -> HRESULT; fn _2(&self) -> HRESULT; fn _3(&self) -> HRESULT;
    fn _4(&self) -> HRESULT; fn _5(&self) -> HRESULT; fn _6(&self) -> HRESULT; fn _7(&self) -> HRESULT;
    fn _8(&self) -> HRESULT; fn _9(&self) -> HRESULT;
    fn SetDefaultEndpoint(&self, id: PCWSTR, role: ERole) -> HRESULT;
}
const CLSID_POLICY_CONFIG: GUID = GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9);

pub struct Device {
    pub id: String,
    pub name: String,
    pub capture: bool,
    dev: IMMDevice,
}

pub struct Session {
    pub exe: String, // lowercase, "system" for system sounds
    pub pid: u32,
    vol: ISimpleAudioVolume,
    pub meter: Option<IAudioMeterInformation>,
}

pub struct Audio {
    en: IMMDeviceEnumerator,
}

type R<T> = windows::core::Result<T>;

impl Audio {
    pub fn new() -> R<Self> {
        Ok(Audio { en: unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)? } })
    }

    pub fn devices(&self) -> Vec<Device> {
        let mut out = vec![];
        unsafe {
            let Ok(col) = self.en.EnumAudioEndpoints(eAll, DEVICE_STATE_ACTIVE) else { return out };
            for i in 0..col.GetCount().unwrap_or(0) {
                let Ok(dev) = col.Item(i) else { continue };
                let Ok(id) = dev.GetId() else { continue };
                let id_s = id.to_string().unwrap_or_default();
                CoTaskMemFree(Some(id.0 as _));
                let name = dev.OpenPropertyStore(STGM_READ)
                    .and_then(|ps| ps.GetValue(&PKEY_Device_FriendlyName))
                    .map(|v| v.to_string())
                    .unwrap_or_default();
                let capture = dev.cast::<IMMEndpoint>().and_then(|e| e.GetDataFlow()).map(|f| f == eCapture).unwrap_or(false);
                out.push(Device { id: id_s, name, capture, dev });
            }
        }
        out
    }

    /// "default", "default_capture", "default_comm", "default_comm_capture", a device id, or part of a name.
    pub fn find(&self, spec: &str) -> Option<Device> {
        let spec = spec.trim().to_lowercase();
        let default = match spec.as_str() {
            "" | "default" => Some((eRender, eConsole)),
            "default_capture" => Some((eCapture, eConsole)),
            "default_comm" => Some((eRender, eCommunications)),
            "default_comm_capture" => Some((eCapture, eCommunications)),
            _ => None,
        };
        if let Some((flow, role)) = default {
            let dev = unsafe { self.en.GetDefaultAudioEndpoint(flow, role).ok()? };
            let id = unsafe { dev.GetId().ok()? };
            let id_s = unsafe { id.to_string().unwrap_or_default() };
            unsafe { CoTaskMemFree(Some(id.0 as _)) };
            return self.devices().into_iter().find(|d| d.id == id_s);
        }
        let all = self.devices();
        let pos = all.iter().position(|d| d.id.to_lowercase() == spec)
            .or_else(|| all.iter().position(|d| d.name.to_lowercase().contains(&spec)))?;
        all.into_iter().nth(pos)
    }

    fn endpoint(&self, spec: &str) -> Option<IAudioEndpointVolume> {
        unsafe { self.find(spec)?.dev.Activate(CLSCTX_ALL, None).ok() }
    }

    pub fn set_device_volume(&self, spec: &str, v: f32) -> R<()> {
        let ep = self.endpoint(spec).ok_or_else(not_found)?;
        unsafe { ep.SetMasterVolumeLevelScalar(v.clamp(0.0, 1.0), std::ptr::null()) }
    }

    /// Live peak meter of a device (what it's playing / what the mic hears).
    pub fn device_meter(&self, spec: &str) -> Option<IAudioMeterInformation> {
        unsafe { self.find(spec)?.dev.Activate(CLSCTX_ALL, None).ok() }
    }

    pub fn device_volume(&self, spec: &str) -> Option<f32> {
        unsafe { self.endpoint(spec)?.GetMasterVolumeLevelScalar().ok() }
    }

    pub fn device_muted(&self, spec: &str) -> Option<bool> {
        unsafe { self.endpoint(spec)?.GetMute().ok().map(|b| b.as_bool()) }
    }

    pub fn toggle_device_mute(&self, spec: &str) -> R<()> {
        let ep = self.endpoint(spec).ok_or_else(not_found)?;
        unsafe { ep.SetMute(!ep.GetMute()?.as_bool(), std::ptr::null()) }
    }

    pub fn set_default(&self, spec: &str) -> R<()> {
        let dev = self.find(spec).ok_or_else(not_found)?;
        let id: Vec<u16> = dev.id.encode_utf16().chain([0]).collect();
        unsafe {
            let pc: IPolicyConfig = CoCreateInstance(&CLSID_POLICY_CONFIG, None, CLSCTX_ALL)?;
            for role in [eConsole, eMultimedia, eCommunications] {
                pc.SetDefaultEndpoint(PCWSTR(id.as_ptr()), role).ok()?;
            }
        }
        Ok(())
    }

    /// Id of the current default device for the same direction as `spec`'s device.
    pub fn default_id(&self, capture: bool) -> Option<String> {
        self.find(if capture { "default_capture" } else { "default" }).map(|d| d.id)
    }

    /// Audio sessions on every active playback device.
    pub fn sessions(&self) -> Vec<Session> {
        let mut out = vec![];
        for d in self.devices().into_iter().filter(|d| !d.capture) {
            unsafe {
                let Ok(mgr) = d.dev.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) else { continue };
                let Ok(en) = mgr.GetSessionEnumerator() else { continue };
                for i in 0..en.GetCount().unwrap_or(0) {
                    let Ok(ctl) = en.GetSession(i) else { continue };
                    if ctl.GetState().map(|s| s == AudioSessionStateExpired).unwrap_or(true) { continue }
                    let Ok(ctl2) = ctl.cast::<IAudioSessionControl2>() else { continue };
                    let pid = ctl2.GetProcessId().unwrap_or(0);
                    let exe = if ctl2.IsSystemSoundsSession() == S_OK { "system".to_string() } else { process_name(pid) };
                    let Ok(vol) = ctl.cast::<ISimpleAudioVolume>() else { continue };
                    let meter = ctl.cast::<IAudioMeterInformation>().ok();
                    out.push(Session { exe, pid, vol, meter });
                }
            }
        }
        out
    }

    /// Process ids with a live audio session on any playback device.
    pub fn session_pids(&self) -> Vec<u32> {
        self.sessions().into_iter().map(|s| s.pid).filter(|&p| p != 0).collect()
    }
}

impl Session {
    pub fn set_volume(&self, v: f32) {
        unsafe { let _ = self.vol.SetMasterVolume(v.clamp(0.0, 1.0), std::ptr::null()); }
    }
    pub fn volume(&self) -> f32 {
        unsafe { self.vol.GetMasterVolume().unwrap_or(0.0) }
    }
    pub fn muted(&self) -> bool {
        unsafe { self.vol.GetMute().map(|b| b.as_bool()).unwrap_or(false) }
    }
    pub fn set_mute(&self, m: bool) {
        unsafe { let _ = self.vol.SetMute(m, std::ptr::null()); }
    }
}

fn not_found() -> windows::core::Error {
    windows::core::Error::new(HRESULT(0x80070490u32 as i32), "audio device not found")
}

/// Per-app output device, as in Settings > "App volume and device preferences".
/// Undocumented WinRT factory (the one EarTrumpet uses); its IID changed in build 21390, so try both.
/// `device_id` None hands the apps back to the Windows default.
pub fn set_app_output(pids: &[u32], device_id: Option<&str>) -> R<()> {
    use std::ffi::c_void;
    use windows::core::{IInspectable, HSTRING};
    const IIDS: [GUID; 2] = [
        GUID::from_u128(0xab3d4648_e242_459f_b02f_541c70306324),
        GUID::from_u128(0x2a59116d_6c4f_45e0_a74f_707e3fef9258),
    ];
    // Vtable: IInspectable (6) + 19 methods we don't use, then SetPersistedDefaultAudioEndpoint.
    const SET_SLOT: usize = 25;
    type SetFn = unsafe extern "system" fn(*mut c_void, u32, EDataFlow, ERole, *mut c_void) -> HRESULT;
    unsafe {
        let factory: IInspectable =
            windows::Win32::System::WinRT::RoGetActivationFactory(&HSTRING::from("Windows.Media.Internal.AudioPolicyConfig"))?;
        let mut raw = std::ptr::null_mut();
        if !IIDS.iter().any(|iid| factory.query(iid, &mut raw).is_ok()) {
            return Err(windows::core::Error::new(HRESULT(0x80004002u32 as i32), "per-app audio routing isn't available on this Windows version"));
        }
        let obj = IUnknown::from_raw(raw); // released on drop
        let set: SetFn = std::mem::transmute(*(*(obj.as_raw() as *const *const usize)).add(SET_SLOT));
        let full = device_id.map(|id| HSTRING::from(format!(r"\\?\SWD#MMDEVAPI#{id}#{{e6327cad-dcec-4949-ae8a-991e976a79d2}}")));
        let arg = full.as_ref().map_or(std::ptr::null_mut(), |h| std::mem::transmute_copy::<HSTRING, *mut c_void>(h));
        for &pid in pids {
            for role in [eConsole, eMultimedia] {
                set(obj.as_raw(), pid, eRender, role, arg).ok()?;
            }
        }
    }
    Ok(())
}

/// Lowercase exe file name of a process, "" if unknown.
pub fn process_name(pid: u32) -> String {
    process_path(pid).rsplit('\\').next().unwrap_or("").to_lowercase()
}

/// Full exe path of a process, "" if unknown.
pub fn process_path(pid: u32) -> String {
    if pid == 0 {
        return String::new();
    }
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return String::new() };
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
        let _ = CloseHandle(h);
        if !ok {
            return String::new();
        }
        String::from_utf16_lossy(&buf[..len as usize])
    }
}

pub fn foreground_exe() -> String {
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(GetForegroundWindow(), Some(&mut pid));
        process_name(pid)
    }
}

//! Querying and changing the display mode via the GDI display-settings API.

use std::fmt;

use windows::Win32::Graphics::Gdi::{
    CDS_TEST, CDS_TYPE, CDS_UPDATEREGISTRY, ChangeDisplaySettingsExW, DEVMODE_FIELD_FLAGS,
    DEVMODEW, DISP_CHANGE, DISP_CHANGE_BADDUALVIEW, DISP_CHANGE_BADFLAGS, DISP_CHANGE_BADMODE,
    DISP_CHANGE_BADPARAM, DISP_CHANGE_FAILED, DISP_CHANGE_NOTUPDATED, DISP_CHANGE_RESTART,
    DISP_CHANGE_SUCCESSFUL, DISPLAY_DEVICE_ATTACHED_TO_DESKTOP, DISPLAY_DEVICE_PRIMARY_DEVICE,
    DISPLAY_DEVICEW, DM_BITSPERPEL, DM_DISPLAYFREQUENCY, DM_PELSHEIGHT, DM_PELSWIDTH,
    ENUM_CURRENT_SETTINGS, ENUM_DISPLAY_SETTINGS_MODE, EnumDisplayDevicesW, EnumDisplaySettingsW,
};
use windows::core::PCWSTR;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Mode {
    pub width: u32,
    pub height: u32,
    pub bpp: u32,
    pub refresh: u32,
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}x{} {}bpp @{}Hz",
            self.width, self.height, self.bpp, self.refresh
        )
    }
}

/// The resolution we want to enforce. `bpp`/`refresh` of 0 mean "don't care".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target {
    pub width: u32,
    pub height: u32,
    pub bpp: u32,
    pub refresh: u32,
}

impl Target {
    pub fn matches(&self, m: &Mode) -> bool {
        m.width == self.width
            && m.height == self.height
            && (self.bpp == 0 || m.bpp == self.bpp)
            && (self.refresh == 0 || m.refresh == self.refresh)
    }
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}x{}", self.width, self.height)?;
        if self.bpp != 0 {
            write!(f, " {}bpp", self.bpp)?;
        }
        if self.refresh != 0 {
            write!(f, " @{}Hz", self.refresh)?;
        }
        Ok(())
    }
}

/// A NUL-terminated UTF-16 display device name, e.g. `\\.\DISPLAY1`.
#[derive(Clone, PartialEq, Eq)]
pub struct Device(Vec<u16>);

impl Device {
    fn from_wide(buf: &[u16]) -> Device {
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        let mut v = buf[..len].to_vec();
        v.push(0);
        Device(v)
    }
    pub fn from_name(s: &str) -> Device {
        Device(s.encode_utf16().chain(std::iter::once(0)).collect())
    }
    fn pcwstr(&self) -> PCWSTR {
        PCWSTR(self.0.as_ptr())
    }
}

impl fmt::Display for Device {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&String::from_utf16_lossy(&self.0[..self.0.len() - 1]))
    }
}

pub struct DeviceInfo {
    pub device: Device,
    pub description: String,
    pub primary: bool,
    pub attached: bool,
}

fn wide_to_string(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

/// All display adapters' outputs known to GDI.
pub fn devices() -> Vec<DeviceInfo> {
    let mut out = Vec::new();
    for i in 0.. {
        let mut dd = DISPLAY_DEVICEW {
            cb: size_of::<DISPLAY_DEVICEW>() as u32,
            ..Default::default()
        };
        if !unsafe { EnumDisplayDevicesW(PCWSTR::null(), i, &mut dd, 0) }.as_bool() {
            break;
        }
        out.push(DeviceInfo {
            device: Device::from_wide(&dd.DeviceName),
            description: wide_to_string(&dd.DeviceString),
            primary: dd.StateFlags.contains(DISPLAY_DEVICE_PRIMARY_DEVICE),
            attached: dd.StateFlags.contains(DISPLAY_DEVICE_ATTACHED_TO_DESKTOP),
        });
    }
    out
}

/// Resolve the configured device name, or the current primary display when empty.
/// Re-resolved on every check because VMware topology changes can move the primary.
pub fn resolve(configured: &str) -> Result<Device, String> {
    if !configured.is_empty() {
        return Ok(Device::from_name(configured));
    }
    devices()
        .into_iter()
        .find(|d| d.primary)
        .map(|d| d.device)
        .ok_or_else(|| "no primary display device found".to_string())
}

fn new_devmode() -> DEVMODEW {
    DEVMODEW {
        dmSize: size_of::<DEVMODEW>() as u16,
        ..Default::default()
    }
}

fn to_mode(dm: &DEVMODEW) -> Mode {
    Mode {
        width: dm.dmPelsWidth,
        height: dm.dmPelsHeight,
        bpp: dm.dmBitsPerPel,
        refresh: dm.dmDisplayFrequency,
    }
}

pub fn current_mode(dev: &Device) -> Result<Mode, String> {
    let mut dm = new_devmode();
    if unsafe { EnumDisplaySettingsW(dev.pcwstr(), ENUM_CURRENT_SETTINGS, &mut dm) }.as_bool() {
        Ok(to_mode(&dm))
    } else {
        Err(format!("EnumDisplaySettingsW(CURRENT) failed for {dev}"))
    }
}

/// Modes the driver advertises for this device (deduplicated, sorted).
pub fn list_modes(dev: &Device) -> Vec<Mode> {
    let mut modes = Vec::new();
    for i in 0.. {
        let mut dm = new_devmode();
        if !unsafe { EnumDisplaySettingsW(dev.pcwstr(), ENUM_DISPLAY_SETTINGS_MODE(i), &mut dm) }
            .as_bool()
        {
            break;
        }
        modes.push(to_mode(&dm));
    }
    modes.sort();
    modes.dedup();
    modes
}

fn disp_change_text(r: DISP_CHANGE) -> &'static str {
    match r {
        DISP_CHANGE_SUCCESSFUL => "successful",
        DISP_CHANGE_RESTART => "restart required",
        DISP_CHANGE_FAILED => "driver failed the mode",
        DISP_CHANGE_BADMODE => "mode not supported",
        DISP_CHANGE_NOTUPDATED => "unable to write registry",
        DISP_CHANGE_BADFLAGS => "bad flags",
        DISP_CHANGE_BADPARAM => "bad parameter",
        DISP_CHANGE_BADDUALVIEW => "bad dual view",
        _ => "unknown error",
    }
}

#[derive(Debug)]
pub struct ApplyError {
    /// The driver rejected the mode in a CDS_TEST dry run: retrying is pointless
    /// until something about the display changes.
    pub rejected: bool,
    pub message: String,
}

impl fmt::Display for ApplyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

/// Switch `dev` to `target`. Only the fields that are set in `target` are changed.
/// The request is validated with CDS_TEST before it is applied.
pub fn apply(dev: &Device, target: &Target, persist: bool) -> Result<(), ApplyError> {
    // Start from the current DEVMODE (as the docs recommend) and mark only the
    // fields we want changed.
    let mut dm = new_devmode();
    if !unsafe { EnumDisplaySettingsW(dev.pcwstr(), ENUM_CURRENT_SETTINGS, &mut dm) }.as_bool() {
        dm = new_devmode();
    }
    let mut fields = DM_PELSWIDTH | DM_PELSHEIGHT;
    dm.dmPelsWidth = target.width;
    dm.dmPelsHeight = target.height;
    if target.bpp != 0 {
        dm.dmBitsPerPel = target.bpp;
        fields |= DM_BITSPERPEL;
    }
    if target.refresh != 0 {
        dm.dmDisplayFrequency = target.refresh;
        fields |= DM_DISPLAYFREQUENCY;
    }
    dm.dmFields = DEVMODE_FIELD_FLAGS(fields.0);

    let test = unsafe { ChangeDisplaySettingsExW(dev.pcwstr(), Some(&dm), None, CDS_TEST, None) };
    if test != DISP_CHANGE_SUCCESSFUL {
        return Err(ApplyError {
            rejected: true,
            message: format!(
                "driver rejected {target} on {dev} (CDS_TEST: {} {})",
                test.0,
                disp_change_text(test)
            ),
        });
    }
    let flags = if persist {
        CDS_UPDATEREGISTRY
    } else {
        CDS_TYPE(0)
    };
    let r = unsafe { ChangeDisplaySettingsExW(dev.pcwstr(), Some(&dm), None, flags, None) };
    if r == DISP_CHANGE_SUCCESSFUL {
        Ok(())
    } else {
        Err(ApplyError {
            rejected: false,
            message: format!(
                "ChangeDisplaySettingsExW({dev}, {target}) returned {} ({})",
                r.0,
                disp_change_text(r)
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_matching() {
        let m = Mode {
            width: 1920,
            height: 1080,
            bpp: 32,
            refresh: 60,
        };
        assert!(
            Target {
                width: 1920,
                height: 1080,
                bpp: 0,
                refresh: 0
            }
            .matches(&m)
        );
        assert!(
            Target {
                width: 1920,
                height: 1080,
                bpp: 32,
                refresh: 60
            }
            .matches(&m)
        );
        assert!(
            !Target {
                width: 1920,
                height: 1080,
                bpp: 16,
                refresh: 0
            }
            .matches(&m)
        );
        assert!(
            !Target {
                width: 1280,
                height: 1080,
                bpp: 0,
                refresh: 0
            }
            .matches(&m)
        );
    }

    #[test]
    fn device_name_roundtrip() {
        let d = Device::from_name(r"\\.\DISPLAY1");
        assert_eq!(d.to_string(), r"\\.\DISPLAY1");
    }
}

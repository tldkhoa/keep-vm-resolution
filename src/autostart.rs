//! Start-at-logon via the `...\CurrentVersion\Run` registry key.
//! HKCU = current user only (no admin); HKLM = every user who logs on (admin).

use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW,
    RegGetValueW, RegSetKeyValueW,
};
use windows::core::{PCWSTR, w};

const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const VALUE_NAME: PCWSTR = w!("KeepVMResolution");

fn root(all_users: bool) -> HKEY {
    if all_users {
        HKEY_LOCAL_MACHINE
    } else {
        HKEY_CURRENT_USER
    }
}

fn scope(all_users: bool) -> &'static str {
    if all_users {
        r"HKLM\...\Run (all users)"
    } else {
        r"HKCU\...\Run (current user)"
    }
}

pub fn install(all_users: bool, command_line: &str) -> Result<String, String> {
    let data: Vec<u16> = command_line
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let r = unsafe {
        RegSetKeyValueW(
            root(all_users),
            RUN_KEY,
            VALUE_NAME,
            REG_SZ.0,
            Some(data.as_ptr().cast()),
            (data.len() * 2) as u32,
        )
    };
    if r != ERROR_SUCCESS {
        let hint = if all_users {
            " (run from an elevated prompt for --all-users)"
        } else {
            ""
        };
        return Err(format!(
            "writing {} failed: {}{hint}",
            scope(all_users),
            r.to_hresult().message()
        ));
    }
    Ok(format!(
        "autostart registered in {}: {command_line}",
        scope(all_users)
    ))
}

pub fn uninstall(all_users: bool) -> Result<String, String> {
    let r = unsafe { RegDeleteKeyValueW(root(all_users), RUN_KEY, VALUE_NAME) };
    if r == ERROR_SUCCESS {
        Ok(format!("autostart removed from {}", scope(all_users)))
    } else if r == ERROR_FILE_NOT_FOUND {
        Ok(format!("no autostart entry in {}", scope(all_users)))
    } else {
        Err(format!(
            "removing from {} failed: {}",
            scope(all_users),
            r.to_hresult().message()
        ))
    }
}

pub fn query(all_users: bool) -> Option<String> {
    let mut buf = vec![0u16; 2048];
    let mut size = (buf.len() * 2) as u32;
    let r = unsafe {
        RegGetValueW(
            root(all_users),
            RUN_KEY,
            VALUE_NAME,
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    if r != ERROR_SUCCESS {
        return None;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..len]))
}

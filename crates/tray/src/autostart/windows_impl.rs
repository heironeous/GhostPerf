//! Real Windows autostart via `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`.
//! Verified with `cargo check --target x86_64-pc-windows-gnu` (no MSVC
//! `lib.exe` in this container to check the `msvc` triple, see TODO.md) —
//! needs the same first-real-run shakeout the agent got in Phase 1/2.

use std::path::PathBuf;

use windows::core::PCWSTR;
use windows::Win32::System::Registry::{
    RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY,
    HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_SAM_FLAGS, REG_SZ,
};

use super::format::{matches_current, run_value_for, VALUE_NAME};

const RUN_SUBKEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn open_run_key(access: REG_SAM_FLAGS) -> anyhow::Result<HKEY> {
    let subkey = wide(RUN_SUBKEY);
    let mut key = HKEY::default();
    let r = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(subkey.as_ptr()),
            Some(0),
            access,
            &mut key,
        )
    };
    if r.0 != 0 {
        anyhow::bail!("failed to open HKCU\\{RUN_SUBKEY}: error {}", r.0);
    }
    Ok(key)
}

pub fn enable() -> anyhow::Result<()> {
    let exe: PathBuf = std::env::current_exe()?;
    let value = run_value_for(&exe);

    let key = open_run_key(KEY_WRITE)?;
    let name_w = wide(VALUE_NAME);
    let value_w = wide(&value);
    // SAFETY: `value_w` outlives this call and its byte length is exactly
    // `value_w.len() * 2`, matching a `Vec<u16>`'s in-memory representation.
    let bytes =
        unsafe { std::slice::from_raw_parts(value_w.as_ptr() as *const u8, value_w.len() * 2) };
    let r = unsafe { RegSetValueExW(key, PCWSTR(name_w.as_ptr()), Some(0), REG_SZ, Some(bytes)) };
    let _ = unsafe { RegCloseKey(key) };

    if r.0 != 0 {
        anyhow::bail!("failed to write {VALUE_NAME} Run value: error {}", r.0);
    }
    Ok(())
}

pub fn disable() -> anyhow::Result<()> {
    let key = open_run_key(KEY_WRITE)?;
    let name_w = wide(VALUE_NAME);
    // Deleting a value that doesn't exist isn't an error — "disable" is
    // idempotent, so a double-click or a fresh install both just work.
    let _ = unsafe { RegDeleteValueW(key, PCWSTR(name_w.as_ptr())) };
    let _ = unsafe { RegCloseKey(key) };
    Ok(())
}

pub fn is_enabled() -> anyhow::Result<bool> {
    let exe = std::env::current_exe()?;
    let key = open_run_key(KEY_READ)?;
    let name_w = wide(VALUE_NAME);

    let mut buf = vec![0u8; 1024];
    let mut size = buf.len() as u32;
    let mut kind = Default::default();
    let r = unsafe {
        RegQueryValueExW(
            key,
            PCWSTR(name_w.as_ptr()),
            None,
            Some(&mut kind),
            Some(buf.as_mut_ptr()),
            Some(&mut size),
        )
    };
    let _ = unsafe { RegCloseKey(key) };

    if r.0 != 0 {
        return Ok(false); // no value at all => not enabled
    }
    let words =
        unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const u16, (size / 2) as usize) };
    let existing = String::from_utf16_lossy(words)
        .trim_end_matches('\0')
        .to_string();
    Ok(matches_current(&existing, &exe))
}

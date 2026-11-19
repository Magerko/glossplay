//! Optional Windows Explorer integration: an "Open with" entry and a context-menu item.
//! Everything goes under HKCU\Software\Classes, so no administrator rights are needed, and
//! the player never becomes the default program for anything.

#![cfg_attr(not(windows), allow(dead_code))]

const VIDEO: &[&str] = &[
    "mkv", "mp4", "m4v", "webm", "avi", "mov", "wmv", "flv", "ts", "m2ts", "mts", "mpg", "mpeg",
    "ogv", "3gp",
];
const AUDIO: &[&str] = &["mp3", "flac", "m4a", "aac", "ogg", "opus", "wav", "wma"];

const APP_KEY: &str = "Applications\\glossplay.exe";
const VERB: &str = "Glossplay";

use crate::i18n::{Tr, from_langid, text_in};

fn all_extensions() -> impl Iterator<Item = &'static str> {
    VIDEO.iter().chain(AUDIO).copied()
}

/// Menu caption for the system UI language (a Windows LANGID). Chinese is not an interface
/// language of the player, so its caption lives here rather than among the interface texts.
pub fn label_for(lang_id: u16) -> &'static str {
    if lang_id & 0x3ff == 0x04 {
        return "用 Glossplay 播放";
    }
    text_in(from_langid(lang_id), Tr::ShellPlayWith)
}

/// Keys (relative to Software\Classes) that hold the context-menu verb. The generic
/// video/audio keys always get one; an extension gets its own only when the system does
/// not already file it under video or audio.
fn verb_keys(perceived: &[(&str, Option<String>)]) -> Vec<String> {
    let mut keys = vec![
        format!("SystemFileAssociations\\video\\shell\\{VERB}"),
        format!("SystemFileAssociations\\audio\\shell\\{VERB}"),
    ];
    for (ext, kind) in perceived {
        let known = kind
            .as_deref()
            .is_some_and(|k| k.eq_ignore_ascii_case("video") || k.eq_ignore_ascii_case("audio"));
        let key = format!("SystemFileAssociations\\.{ext}\\shell\\{VERB}");
        if !known && !keys.contains(&key) {
            keys.push(key);
        }
    }
    keys
}

#[cfg(windows)]
mod imp {
    use super::*;
    use crate::i18n::system_langid;
    use std::ptr::{null, null_mut};
    use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
    use windows_sys::Win32::System::Registry::*;
    use windows_sys::Win32::UI::Shell::{SHCNE_ASSOCCHANGED, SHCNF_IDLIST, SHChangeNotify};

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    fn check(code: u32, what: &str) -> Result<(), String> {
        if code == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(format!("{what}: Windows error {code}"))
        }
    }

    fn classes(key: &str) -> Vec<u16> {
        wide(&format!("Software\\Classes\\{key}"))
    }

    fn set(key: &str, name: Option<&str>, value: &str) -> Result<(), String> {
        let mut h: HKEY = null_mut();
        let path = classes(key);
        let rc = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                path.as_ptr(),
                0,
                null(),
                0,
                KEY_WRITE,
                null(),
                &mut h,
                null_mut(),
            )
        };
        check(rc, &format!("create {key}"))?;
        let name = name.map(wide);
        let data = wide(value);
        let rc = unsafe {
            RegSetValueExW(
                h,
                name.as_ref().map_or(null(), |n| n.as_ptr()),
                0,
                REG_SZ,
                data.as_ptr().cast(),
                (data.len() * 2) as u32,
            )
        };
        unsafe { RegCloseKey(h) };
        check(rc, &format!("write {key}"))
    }

    fn get(root: HKEY, key: &str, name: Option<&str>) -> Option<String> {
        let key = wide(key);
        let name = name.map(wide);
        let name_ptr = name.as_ref().map_or(null(), |n| n.as_ptr());
        let mut bytes = 0u32;
        let rc = unsafe {
            RegGetValueW(
                root,
                key.as_ptr(),
                name_ptr,
                RRF_RT_REG_SZ,
                null_mut(),
                null_mut(),
                &mut bytes,
            )
        };
        if rc != ERROR_SUCCESS {
            return None;
        }
        let mut buf = vec![0u16; (bytes as usize).div_ceil(2)];
        let rc = unsafe {
            RegGetValueW(
                root,
                key.as_ptr(),
                name_ptr,
                RRF_RT_REG_SZ,
                null_mut(),
                buf.as_mut_ptr().cast(),
                &mut bytes,
            )
        };
        if rc != ERROR_SUCCESS {
            return None;
        }
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Some(String::from_utf16_lossy(&buf[..len]))
    }

    fn delete_tree(key: &str) -> Result<(), String> {
        let path = classes(key);
        match unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, path.as_ptr()) } {
            ERROR_FILE_NOT_FOUND => Ok(()),
            rc => check(rc, &format!("delete {key}")),
        }
    }

    /// Removes a parent I may have created, but only if it has no subkeys and no values.
    fn delete_if_empty(key: &str) {
        let path = classes(key);
        let mut h: HKEY = null_mut();
        if unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, path.as_ptr(), 0, KEY_READ, &mut h) }
            != ERROR_SUCCESS
        {
            return;
        }
        let (mut subkeys, mut values) = (0u32, 0u32);
        let rc = unsafe {
            RegQueryInfoKeyW(
                h,
                null_mut(),
                null_mut(),
                null(),
                &mut subkeys,
                null_mut(),
                null_mut(),
                &mut values,
                null_mut(),
                null_mut(),
                null_mut(),
                null_mut(),
            )
        };
        unsafe { RegCloseKey(h) };
        if rc == ERROR_SUCCESS && subkeys == 0 && values == 0 {
            unsafe { RegDeleteKeyW(HKEY_CURRENT_USER, path.as_ptr()) };
        }
    }

    fn perceived_type(ext: &str) -> Option<String> {
        get(HKEY_CLASSES_ROOT, &format!(".{ext}"), Some("PerceivedType")).or_else(|| {
            get(
                HKEY_CLASSES_ROOT,
                &format!("SystemFileAssociations\\.{ext}"),
                Some("PerceivedType"),
            )
        })
    }

    fn exe_path() -> Result<String, String> {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let exe = exe.to_string_lossy();
        // Explorer does not always understand the \\?\ prefix inside commands.
        Ok(exe.strip_prefix(r"\\?\").unwrap_or(&exe).to_owned())
    }

    fn command_for(exe: &str) -> String {
        format!("\"{exe}\" \"%1\"")
    }

    fn notify() {
        unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED as i32, SHCNF_IDLIST, null(), null()) };
    }

    pub fn register() -> Result<String, String> {
        let exe = exe_path()?;
        let command = command_for(&exe);
        let label = label_for(system_langid());

        set(APP_KEY, Some("FriendlyAppName"), "Glossplay")?;
        set(&format!("{APP_KEY}\\shell\\open\\command"), None, &command)?;
        for ext in all_extensions() {
            set(
                &format!("{APP_KEY}\\SupportedTypes"),
                Some(&format!(".{ext}")),
                "",
            )?;
        }

        let perceived: Vec<_> = all_extensions().map(|e| (e, perceived_type(e))).collect();
        for key in verb_keys(&perceived) {
            set(&key, None, label)?;
            set(&key, Some("MUIVerb"), label)?;
            set(&key, Some("Icon"), &format!("\"{exe}\",0"))?;
            set(&format!("{key}\\command"), None, &command)?;
        }
        notify();
        Ok(format!("registered {exe} (menu text: {label})"))
    }

    pub fn unregister() -> Result<String, String> {
        delete_tree(APP_KEY)?;
        // I do not know which extensions needed their own key when I registered, so I
        // clear every candidate; missing ones are fine.
        let mut parents = vec![
            "SystemFileAssociations\\video".to_owned(),
            "SystemFileAssociations\\audio".to_owned(),
        ];
        parents.extend(all_extensions().map(|e| format!("SystemFileAssociations\\.{e}")));
        for parent in &parents {
            delete_tree(&format!("{parent}\\shell\\{VERB}"))?;
            delete_if_empty(&format!("{parent}\\shell"));
            delete_if_empty(parent);
        }
        notify();
        Ok("unregistered".into())
    }

    /// Re-registers when the stored command points somewhere else, e.g. after the player
    /// folder was moved. A missing registration counts as stale too.
    pub fn refresh_if_stale() -> Result<bool, String> {
        let want = command_for(&exe_path()?);
        let key = format!("Software\\Classes\\{APP_KEY}\\shell\\open\\command");
        if get(HKEY_CURRENT_USER, &key, None).as_deref() == Some(want.as_str()) {
            return Ok(false);
        }
        register().map(|_| true)
    }
}

#[cfg(not(windows))]
mod imp {
    const NO: &str = "not supported on this platform";
    pub fn register() -> Result<String, String> {
        Err(NO.into())
    }
    pub fn unregister() -> Result<String, String> {
        Err(NO.into())
    }
    pub fn refresh_if_stale() -> Result<bool, String> {
        Err(NO.into())
    }
}

pub use imp::{refresh_if_stale, register, unregister};

pub const SUPPORTED: bool = cfg!(windows);

/// Handles `--shell-register` / `--shell-unregister`; returns the exit code.
pub fn cli(register_it: bool) -> i32 {
    let result = if register_it {
        register()
    } else {
        unregister()
    };
    match result {
        Ok(text) => {
            println!("{text}");
            0
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_follow_the_primary_language() {
        assert_eq!(label_for(0x0419), "Воспроизвести в Glossplay");
        assert_eq!(label_for(0x0422), "Відтворити в Glossplay");
        assert_eq!(label_for(0x0804), "用 Glossplay 播放");
        assert_eq!(label_for(0x0404), "用 Glossplay 播放");
        assert_eq!(label_for(0x0409), "Play with Glossplay");
        assert_eq!(label_for(0x0407), "Play with Glossplay");
        assert_eq!(label_for(0), "Play with Glossplay");
    }

    #[test]
    fn extension_keys_only_where_type_is_unknown() {
        let perceived = [
            ("mkv", Some("video".to_string())),
            ("flv", None),
            ("opus", Some("Audio".to_string())),
            ("ts", Some("text".to_string())),
            ("flv", None),
        ];
        let keys = verb_keys(&perceived);
        assert_eq!(
            keys,
            [
                "SystemFileAssociations\\video\\shell\\Glossplay",
                "SystemFileAssociations\\audio\\shell\\Glossplay",
                "SystemFileAssociations\\.flv\\shell\\Glossplay",
                "SystemFileAssociations\\.ts\\shell\\Glossplay",
            ]
        );
    }
}

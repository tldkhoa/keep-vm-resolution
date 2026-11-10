//! Simple `key = value` configuration file stored next to the executable.

use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    /// Preset width in physical pixels.
    pub width: u32,
    /// Preset height in physical pixels.
    pub height: u32,
    /// Colour depth to enforce; 0 = keep whatever is current.
    pub bpp: u32,
    /// Refresh rate (Hz) to enforce; 0 = keep whatever is current.
    pub refresh: u32,
    /// Display device name such as `\\.\DISPLAY1`; empty = primary display.
    pub device: String,
    /// Wait this long after the last display change before restoring (debounce).
    pub settle_ms: u32,
    /// Fallback periodic check interval in seconds; 0 disables polling.
    pub poll_secs: u32,
    /// At most this many restores within `rate_window_secs` ...
    pub max_restores: u32,
    pub rate_window_secs: u32,
    /// ... otherwise pause for this long (prevents fighting VMware Tools forever).
    pub cooldown_secs: u32,
    /// Write the mode to the registry (CDS_UPDATEREGISTRY) instead of a dynamic change.
    pub persist: bool,
    /// Do nothing while the session is an RDP session.
    pub skip_remote: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            width: 1920,
            height: 1080,
            bpp: 0,
            refresh: 0,
            device: String::new(),
            settle_ms: 1500,
            poll_secs: 15,
            max_restores: 5,
            rate_window_secs: 60,
            cooldown_secs: 120,
            persist: false,
            skip_remote: true,
        }
    }
}

/// `keep-vm-resolution.ini` next to the executable, shared by the CLI and the
/// background exe.
pub fn default_path() -> PathBuf {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("keep-vm-resolution.exe"));
    default_path_for(&exe)
}

pub fn default_path_for(exe: &Path) -> PathBuf {
    exe.with_file_name("keep-vm-resolution.ini")
}

fn parse_bool(v: &str) -> Option<bool> {
    match v.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Config, String> {
        let text =
            fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        Self::parse(&text)
    }

    pub fn parse(text: &str) -> Result<Config, String> {
        let mut c = Config::default();
        for (n, raw) in text.lines().enumerate() {
            let line = raw.trim().trim_start_matches('\u{feff}');
            if line.is_empty()
                || line.starts_with('#')
                || line.starts_with(';')
                || line.starts_with('[')
            {
                continue;
            }
            let Some((k, v)) = line.split_once('=') else {
                return Err(format!("line {}: expected key = value", n + 1));
            };
            let (k, v) = (k.trim().to_ascii_lowercase(), v.trim());
            let num = |v: &str| {
                v.parse::<u32>()
                    .map_err(|_| format!("line {}: '{k}' must be a non-negative integer", n + 1))
            };
            let boolean = |v: &str| {
                parse_bool(v).ok_or_else(|| format!("line {}: '{k}' must be true/false", n + 1))
            };
            match k.as_str() {
                "width" => c.width = num(v)?,
                "height" => c.height = num(v)?,
                "bpp" | "bits_per_pixel" => c.bpp = num(v)?,
                "refresh" | "refresh_hz" => c.refresh = num(v)?,
                "device" => c.device = v.trim_matches('"').to_string(),
                "settle_ms" => c.settle_ms = num(v)?,
                "poll_secs" => c.poll_secs = num(v)?,
                "max_restores" => c.max_restores = num(v)?,
                "rate_window_secs" => c.rate_window_secs = num(v)?,
                "cooldown_secs" => c.cooldown_secs = num(v)?,
                "persist" => c.persist = boolean(v)?,
                "skip_remote" => c.skip_remote = boolean(v)?,
                _ => return Err(format!("line {}: unknown key '{k}'", n + 1)),
            }
        }
        c.validate()?;
        Ok(c)
    }

    pub fn validate(&self) -> Result<(), String> {
        if !(320..=16384).contains(&self.width) || !(200..=16384).contains(&self.height) {
            return Err(format!(
                "preset {}x{} is out of range",
                self.width, self.height
            ));
        }
        if self.max_restores == 0 {
            return Err("max_restores must be at least 1".into());
        }
        Ok(())
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        let text = format!(
            "# keep-vm-resolution configuration\r\n\
             # Preset resolution that is restored whenever something (e.g. VMware Tools\r\n\
             # autofit from the web console) changes it.\r\n\
             width = {}\r\n\
             height = {}\r\n\
             # 0 = do not enforce colour depth / refresh rate\r\n\
             bpp = {}\r\n\
             refresh = {}\r\n\
             # Display device (e.g. \\\\.\\DISPLAY1). Empty = primary display.\r\n\
             device = {}\r\n\
             \r\n\
             # Wait this long after the last change before restoring (ms).\r\n\
             settle_ms = {}\r\n\
             # Fallback periodic check (seconds, 0 = off).\r\n\
             poll_secs = {}\r\n\
             # Anti fight-loop: at most max_restores per rate_window_secs, then pause cooldown_secs.\r\n\
             max_restores = {}\r\n\
             rate_window_secs = {}\r\n\
             cooldown_secs = {}\r\n\
             # true = also store the mode in the registry (CDS_UPDATEREGISTRY).\r\n\
             persist = {}\r\n\
             # true = do nothing inside RDP sessions (resolution is owned by the RDP client).\r\n\
             skip_remote = {}\r\n",
            self.width,
            self.height,
            self.bpp,
            self.refresh,
            self.device,
            self.settle_ms,
            self.poll_secs,
            self.max_restores,
            self.rate_window_secs,
            self.cooldown_secs,
            self.persist,
            self.skip_remote,
        );
        fs::write(path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let c = Config {
            width: 1280,
            height: 720,
            refresh: 60,
            device: r"\\.\DISPLAY1".into(),
            persist: true,
            ..Default::default()
        };
        let dir = std::env::temp_dir().join(format!("kvr-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("t.ini");
        c.save(&p).unwrap();
        assert_eq!(Config::load(&p).unwrap(), c);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn rejects_unknown_and_bad_values() {
        assert!(Config::parse("widht = 10").is_err());
        assert!(Config::parse("width = abc").is_err());
        assert!(Config::parse("width = 10").is_err());
        assert!(Config::parse("persist = maybe").is_err());
        assert!(Config::parse("max_restores = 0").is_err());
    }

    #[test]
    fn comments_and_defaults() {
        let c = Config::parse("\u{feff}# hi\r\n[main]\r\nwidth=1024\r\nheight = 768\r\n; x\r\n")
            .unwrap();
        assert_eq!((c.width, c.height, c.poll_secs), (1024, 768, 15));
    }
}

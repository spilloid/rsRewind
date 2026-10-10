//! LaunchAgent encoding, without shell command interpolation.
#[cfg(any(target_os = "macos", test))]
use std::path::Path;

#[cfg(any(target_os = "macos", test))]
fn xml(value: &str) -> anyhow::Result<String> {
    if value
        .chars()
        .any(|c| c < ' ' && !matches!(c, '\n' | '\r' | '\t'))
    {
        anyhow::bail!("a login path contains a character not allowed in a plist");
    }
    Ok(value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;"))
}

#[cfg(any(target_os = "macos", test))]
fn plist(exe: &Path, data: &Path) -> anyhow::Result<String> {
    if !exe.is_absolute() || !data.is_absolute() {
        anyhow::bail!("login startup requires absolute executable and data paths");
    }
    let exe = xml(exe
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("the executable path is not UTF-8"))?)?;
    let data = xml(data
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("the data path is not UTF-8"))?)?;
    Ok(format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>us.spillerstech.rsrewind</string>
<key>ProgramArguments</key><array>
<string>{exe}</string><string>--data-dir</string><string>{data}</string>
<string>tray</string><string>--foreground</string><string>--start-recorder</string>
</array>
<key>RunAtLoad</key><true/>
<key>LimitLoadToSessionType</key><string>Aqua</string>
<key>ProcessType</key><string>Interactive</string>
</dict></plist>
"#
    ))
}

#[cfg(target_os = "macos")]
pub fn set(exe: &Path, data: &Path, enabled: bool) -> anyhow::Result<()> {
    use anyhow::Context;
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let home = std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .context("HOME is not set")?;
    let dir = std::path::PathBuf::from(home).join("Library/LaunchAgents");
    let path = dir.join("us.spillerstech.rsrewind.plist");
    if enabled {
        let content = plist(exe, data)?;
        std::fs::create_dir_all(&dir)?;
        // Same-directory rename keeps a partially written login configuration invisible.
        let pending = dir.join(format!(".rsrewind-{}.plist", std::process::id()));
        let result = (|| -> anyhow::Result<()> {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&pending)?;
            file.write_all(content.as_bytes())?;
            file.sync_all()?;
            std::fs::rename(&pending, &path)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&pending);
        }
        result?;
    } else {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn launch_arguments_are_literal_and_preserve_spaces() -> anyhow::Result<()> {
        let text = plist(
            Path::new("/Applications/rsRewind & friends.app/Contents/MacOS/rsrewind"),
            Path::new("/Users/u/Library/Application Support/<history>"),
        )?;
        assert!(text.contains("rsRewind &amp; friends.app"));
        assert!(text.contains("Application Support/&lt;history&gt;"));
        assert!(text.contains("<string>--data-dir</string>"));
        assert!(!text.contains("/bin/sh"));
        assert!(plist(Path::new("relative"), Path::new("/data")).is_err());
        assert!(plist(Path::new("/bin/app"), Path::new("/data/\u{1}")).is_err());
        Ok(())
    }
}

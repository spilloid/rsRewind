//! The application-menu entry on Linux: `rsrewind.desktop` plus its icon, written into the user's
//! XDG data folder so KRunner, the GNOME overview and application menus find "rsRewind" by name.
//!
//! The tarball has no installer, so the first `rsrewind launch` (or `rsrewind tray --autostart on`)
//! adds the entry; it is rewritten whenever the executable moves.
//!
//! An AppImage runs from a temporary mount that is different on every launch and disappears when it
//! exits, so nothing started from it could outlive the window (the tray icon, the recorder) and KWin,
//! which authorizes screenshots by the program's path, could never recognise it. So the first run
//! copies the program to a fixed place and re-runs itself from there; later runs refresh the copy
//! when the AppImage is newer. Everything else (the menu entry, login startup, the recorder's KWin
//! entry) then names that fixed copy.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

const ICON_48: &[u8] = include_bytes!("../../../assets/icon/launcher-48.png");
const ICON_128: &[u8] = include_bytes!("../../../assets/icon/launcher-128.png");
const ICON_256: &[u8] = include_bytes!("../../../assets/icon/launcher-256.png");

/// Desktop-entry text for a launcher that runs `<exe> launch`. A value with a space, quote or
/// backslash is quoted as the Desktop Entry spec requires.
pub fn desktop_entry(exe: &Path, data_dir: &Path) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName=rsRewind\nGenericName=Screen history\n\
         Comment=Search and rewind everything that was on your screen\n\
         Keywords=history;screen;recall;rewind;search;screenshots;\n\
         Exec={} --data-dir {} launch\nIcon=rsrewind\nCategories=Utility;\nTerminal=false\n\
         StartupNotify=false\n",
        exec_arg(&exe.to_string_lossy()),
        exec_arg(&data_dir.to_string_lossy()),
    )
}

fn exec_arg(value: &str) -> String {
    let mut quoted = String::from("\"");
    for ch in value.chars() {
        // Inside double quotes the spec escapes ", `, $ and \ with a backslash; % starts a field code.
        match ch {
            '"' | '`' | '$' | '\\' => {
                quoted.push('\\');
                quoted.push(ch);
            }
            '%' => quoted.push_str("%%"),
            _ => quoted.push(ch),
        }
    }
    quoted.push('"');
    quoted
}

/// True when `exe` is inside an AppImage's temporary mount (`/tmp/.mount_<name><random>/...`). Judged
/// by path alone: programs started from the installed copy inherit `APPIMAGE` in their environment.
fn in_appimage_mount(exe: &Path) -> bool {
    exe.to_string_lossy().contains("/.mount_")
}

/// Where an AppImage keeps the copy of itself that actually runs.
fn installed_copy(home: &Path) -> PathBuf {
    home.join("rsRewind-app/rsrewind")
}

/// Puts `source` at `dest` (atomically, executable) unless `dest` already holds the same bytes.
fn ensure_copy(source: &Path, dest: &Path) -> Result<()> {
    let wanted = std::fs::read(source).with_context(|| format!("read {}", source.display()))?;
    if std::fs::read(dest).is_ok_and(|have| have == wanted) {
        return Ok(());
    }
    let dir = dest.parent().context("installed copy has no folder")?;
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    let staging = dest.with_extension("new");
    std::fs::write(&staging, &wanted).with_context(|| format!("write {}", staging.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o755))
            .with_context(|| format!("make {} executable", staging.display()))?;
    }
    // A running tray or recorder keeps the old file open; the rename leaves it alone.
    std::fs::rename(&staging, dest).with_context(|| format!("replace {}", dest.display()))
}

/// Called first thing in `main`. From an AppImage: install or refresh the fixed copy and replace this
/// process with it (same arguments), so it never returns on success. From anywhere else it does
/// nothing. An error means the copy could not be made; the caller carries on running in place.
pub fn run_from_installed_copy() -> Result<()> {
    use std::os::unix::process::CommandExt;
    let exe = std::env::current_exe().context("find the rsrewind executable")?;
    if !in_appimage_mount(&exe) {
        return Ok(());
    }
    let dest = installed_copy(&data_home()?);
    ensure_copy(&exe, &dest)?;
    let error = std::process::Command::new(&dest)
        .args(std::env::args_os().skip(1))
        .env_remove("APPDIR")
        .env_remove("ARGV0")
        .env_remove("OWD")
        .exec();
    Err(error).with_context(|| format!("run {}", dest.display()))
}

fn data_home() -> Result<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .context("HOME is not set")
}

fn write_if_changed(path: &Path, contents: &[u8]) -> Result<()> {
    if std::fs::read(path).is_ok_and(|existing| existing == contents) {
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    std::fs::write(path, contents).with_context(|| format!("write {}", path.display()))
}

/// Adds or refreshes the menu entry and icons. Cheap and idempotent: unchanged files are not rewritten.
pub fn install(data_dir: &Path) -> Result<()> {
    let exe = std::env::current_exe().context("find the rsrewind executable")?;
    let home = data_home()?;
    for (size, bytes) in [(48, ICON_48), (128, ICON_128), (256, ICON_256)] {
        let path = home.join(format!("icons/hicolor/{size}x{size}/apps/rsrewind.png"));
        write_if_changed(&path, bytes)?;
    }
    let entry = desktop_entry(&exe, data_dir);
    write_if_changed(
        &home.join("applications/rsrewind.desktop"),
        entry.as_bytes(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_runs_launch_and_quotes_paths() -> Result<(), Box<dyn std::error::Error>> {
        let entry = desktop_entry(
            Path::new("/home/me/my apps/rsrewind"),
            Path::new("/home/me/.local/share/rsRewind"),
        );
        assert!(entry.starts_with("[Desktop Entry]\nType=Application\nName=rsRewind\n"));
        assert!(entry.contains(
            "Exec=\"/home/me/my apps/rsrewind\" --data-dir \"/home/me/.local/share/rsRewind\" launch\n"
        ));
        assert!(entry.contains("Icon=rsrewind\n"));
        assert!(entry.contains("Terminal=false\n"));
        // Not hidden: the point is that application search finds it.
        assert!(!entry.contains("NoDisplay"));
        Ok(())
    }

    #[test]
    fn exec_arguments_escape_what_the_spec_requires() {
        assert_eq!(exec_arg(r#"a"b$c`d\e%f"#), r#""a\"b\$c\`d\\e%%f""#);
    }

    #[test]
    fn appimage_mounts_are_recognised_by_path_only() {
        assert!(in_appimage_mount(Path::new(
            "/tmp/.mount_rsRewiAbC123/usr/bin/rsrewind"
        )));
        assert!(!in_appimage_mount(Path::new(
            "/home/me/.local/share/rsRewind-app/rsrewind"
        )));
        assert!(!in_appimage_mount(Path::new("/usr/local/bin/rsrewind")));
    }

    #[test]
    fn the_installed_copy_is_made_refreshed_and_left_alone_when_current()
    -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let source = dir.path().join("mounted-rsrewind");
        let dest = installed_copy(&dir.path().join("share"));
        std::fs::write(&source, b"v1")?;
        ensure_copy(&source, &dest)?;
        assert_eq!(std::fs::read(&dest)?, b"v1");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&dest)?.permissions().mode() & 0o777,
                0o755
            );
        }
        let first = std::fs::metadata(&dest)?.modified()?;
        std::thread::sleep(std::time::Duration::from_millis(20));
        ensure_copy(&source, &dest)?;
        assert_eq!(
            std::fs::metadata(&dest)?.modified()?,
            first,
            "unchanged copy rewritten"
        );
        std::fs::write(&source, b"v2")?;
        ensure_copy(&source, &dest)?;
        assert_eq!(std::fs::read(&dest)?, b"v2");
        assert!(
            !dest.with_extension("new").exists(),
            "staging file left behind"
        );
        Ok(())
    }

    #[test]
    fn write_if_changed_leaves_identical_files_alone() -> Result<(), Box<dyn std::error::Error>> {
        let dir = std::env::temp_dir().join(format!("rsrewind-launcher-{}", std::process::id()));
        let path = dir.join("applications/rsrewind.desktop");
        write_if_changed(&path, b"one")?;
        let first = std::fs::metadata(&path)?.modified()?;
        std::thread::sleep(std::time::Duration::from_millis(20));
        write_if_changed(&path, b"one")?;
        assert_eq!(std::fs::metadata(&path)?.modified()?, first);
        write_if_changed(&path, b"two")?;
        assert_eq!(std::fs::read(&path)?, b"two");
        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }
}

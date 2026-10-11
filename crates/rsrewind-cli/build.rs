//! Windows only: give rsrewind.exe its icon (and the version resource Cargo fills in from the
//! package), so Explorer, the taskbar, the Start menu and Windows search show the rsRewind mark.

fn main() {
    #[cfg(windows)]
    {
        let icon = "../../assets/icon/rsrewind.ico";
        println!("cargo:rerun-if-changed={icon}");
        if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
            let mut resource = winresource::WindowsResource::new();
            resource.set_icon(icon);
            if let Err(error) = resource.compile() {
                // A release without its icon is wrong, not merely plain: fail the build.
                eprintln!("could not embed the application icon: {error}");
                std::process::exit(1);
            }
        }
    }
}

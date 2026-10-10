use std::{env, path::PathBuf, process::Command};

fn output(command: &mut Command) -> String {
    let result = command
        .output()
        .unwrap_or_else(|e| panic!("macOS tray build: {e}"));
    assert!(
        result.status.success(),
        "macOS tray build: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8(result.stdout).unwrap_or_else(|e| panic!("tool output: {e}"))
}

fn main() {
    println!("cargo:rerun-if-changed=native/Tray.swift");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    let out = PathBuf::from(
        env::var_os("OUT_DIR").unwrap_or_else(|| panic!("Cargo did not set OUT_DIR")),
    );
    let arch = env::var("CARGO_CFG_TARGET_ARCH")
        .unwrap_or_else(|e| panic!("Cargo did not set target architecture: {e}"));
    let arch = match arch.as_str() {
        "aarch64" => "arm64",
        "x86_64" => "x86_64",
        _ => panic!("unsupported macOS architecture"),
    };
    let sdk = output(Command::new("xcrun").args(["--sdk", "macosx", "--show-sdk-path"]));
    let swift = output(Command::new("xcrun").args(["--find", "swiftc"]));
    let object = out.join("Tray.o");
    output(
        Command::new(swift.trim())
            .args([
                "-parse-as-library",
                "-emit-object",
                "-O",
                "-sdk",
                sdk.trim(),
                "-target",
                &format!("{arch}-apple-macosx14.0"),
                "native/Tray.swift",
                "-o",
            ])
            .arg(&object),
    );
    let library = out.join("librsrewind_tray_native.a");
    output(
        Command::new("xcrun")
            .args(["ar", "crs"])
            .arg(&library)
            .arg(&object),
    );
    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-lib=static=rsrewind_tray_native");
    let runtime = PathBuf::from(swift.trim())
        .parent()
        .unwrap_or_else(|| panic!("swiftc has no binary directory"))
        .join("../lib/swift/macosx");
    println!("cargo:rustc-link-search=native={}", runtime.display());
    println!("cargo:rustc-link-search=native=/usr/lib/swift");
    println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
    // Apple's linker reads LC_LINKER_OPTION autolink commands directly from Swift objects.
    // swift-autolink-extract is for non-Apple object formats; it is not required by Xcode.
    // https://github.com/swiftlang/swift/blob/main/docs/Driver.md#linking
    println!(
        "cargo:rustc-link-search=native={}/usr/lib/swift",
        sdk.trim()
    );
    println!("cargo:rustc-link-lib=framework=AppKit");
}

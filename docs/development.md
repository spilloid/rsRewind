# Build and contribute

To use rsRewind, start with [installation](https://spilloid.github.io/rsRewind/install.html). These instructions are for developing the application.

Install Rust through rustup; `rust-toolchain.toml` pins the toolchain (Rust 1.95+, edition 2024). Windows needs MSVC Build Tools with Desktop development with C++ and the Windows SDK. Linux recording requires KDE Plasma 6 on Wayland, and OCR requires distribution-provided Tesseract. macOS builds need Apple Silicon, macOS 14+ and Xcode command-line tools; see [macOS](macos.md) for packaging and acceptance.

```sh
cargo build --workspace
cargo test --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo run -p rsrewind-cli -- status
```

The viewer is a native Iced/wgpu window and needs a working GPU driver. See [contributing](../CONTRIBUTING.md), [architecture](../ARCHITECTURE.md) and [review process](dev-process.md). Hosted macOS builds verify compilation and tests; they do not replace real-Mac recording, permission and UI acceptance.

## Linux: supervised recorder service

For a recorder supervised by systemd, use this **instead of** tray autostart. Save as `~/.config/systemd/user/rsrewind.service`:

```ini
[Unit]
Description=rsRewind screen-history recorder
PartOf=graphical-session.target
After=graphical-session.target

[Service]
ExecStart=%h/.local/bin/rsrewind daemon
Restart=on-failure
RestartSec=10

[Install]
WantedBy=graphical-session.target
```

```sh
~/.local/bin/rsrewind tray --autostart off
~/.local/bin/rsrewind stop
systemctl --user daemon-reload
systemctl --user enable --now rsrewind.service
```

The service requires a working graphical Plasma session. The tray remains a separate controller; start it without `--start-recorder` if you want its menu.

//! Live smoke test of the capture crate on the current desktop.
//!
//! `cargo run -p rsrewind-capture --example probe [frames=3] [interval_ms=500]`
//!
//! Prints monitors, the foreground *process* (never the title), visible-window count and idle
//! time, then captures three frames per monitor ~500 ms apart and reports what arrived and the
//! change-detector verdict. The first frame of monitor 0 is written to `%TEMP%` as raw BGRA plus
//! a small text file with its dimensions.

#[cfg(windows)]
use rsrewind_capture::{
    ChangeDetection, ChangeDetector, Fingerprint, MonitorCapturer, enable_dpi_awareness,
    foreground, idle_millis, monitors, visible_windows,
};
#[cfg(windows)]
use std::time::{Duration, Instant};

#[cfg(not(windows))]
fn main() {
    eprintln!("the capture probe uses Windows.Graphics.Capture and runs on Windows only");
}

#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let frames: u32 = args.next().and_then(|a| a.parse().ok()).unwrap_or(3);
    let interval_ms: u64 = args.next().and_then(|a| a.parse().ok()).unwrap_or(500);
    println!("dpi awareness: {:?}", enable_dpi_awareness()?);

    let monitors = monitors()?;
    println!("monitors: {}", monitors.len());
    for (i, (_, info)) in monitors.iter().enumerate() {
        println!(
            "  [{i}] {} {}x{} at ({},{}) dpi={} primary={}",
            info.device_name, info.width, info.height, info.left, info.top, info.dpi, info.primary
        );
    }

    match foreground() {
        Some(focus) => println!(
            "foreground: process={} pid={} exe_path_known={} class={:?} title_chars={}",
            focus.application.process_name,
            focus.pid,
            focus.application.exe_path.is_some(),
            focus.window.class_name,
            focus.window.title.chars().count()
        ),
        None => println!("foreground: none"),
    }

    let windows = visible_windows();
    println!("visible windows: {}", windows.len());
    for (i, (_, info)) in monitors.iter().enumerate() {
        let on = windows
            .iter()
            .filter(|w| w.monitor == info.device_name)
            .count();
        let touching = windows.iter().filter(|w| w.rect.intersects(info)).count();
        println!("  [{i}] mostly-on={on} intersecting={touching}");
    }
    println!("idle ms: {}", idle_millis());

    let detector = ChangeDetector::default();
    for (i, (handle, info)) in monitors.iter().enumerate() {
        println!("monitor [{i}] {}", info.device_name);
        let started = Instant::now();
        let mut capturer = match MonitorCapturer::new(*handle) {
            Ok(capturer) => capturer,
            Err(error) => {
                println!(
                    "  capturer failed: {error} (recoverable={}) source={:?}",
                    error.is_recoverable(),
                    std::error::Error::source(&error)
                );
                continue;
            }
        };
        println!(
            "  capturer up in {} ms adapter={:?} cursor_disabled={} border_disabled={}",
            started.elapsed().as_millis(),
            capturer.adapter(),
            capturer.cursor_disabled(),
            capturer.border_disabled()
        );

        let mut previous: Option<Fingerprint> = None;
        for attempt in 0..frames {
            std::thread::sleep(Duration::from_millis(interval_ms));
            let t = Instant::now();
            match capturer.latest_frame() {
                Ok(Some(frame)) => {
                    let copy_ms = t.elapsed().as_millis();
                    let fingerprint = Fingerprint::from_frame(&frame)?;
                    let verdict = previous.as_ref().map(|p| {
                        (
                            detector.is_meaningful(p, &fingerprint),
                            p.changed_fraction(&fingerprint, detector.epsilon),
                            p.hamming(&fingerprint),
                        )
                    });
                    println!(
                        "  frame {attempt}: new=true {}x{} stride={} bytes={} well_formed={} \
                         copy_ms={copy_ms} dhash={:016x} vs_prev(meaningful,fraction,hamming)={:?}",
                        frame.width,
                        frame.height,
                        frame.stride,
                        frame.pixels.len(),
                        frame.is_well_formed(),
                        fingerprint.dhash(),
                        verdict
                    );
                    if i == 0 && attempt == 0 {
                        let dir = std::env::temp_dir();
                        let raw = dir.join("rsrewind-probe-m0.bgra");
                        std::fs::write(&raw, &frame.pixels)?;
                        std::fs::write(
                            dir.join("rsrewind-probe-m0.txt"),
                            format!(
                                "width={}\nheight={}\nstride={}\nformat=BGRA8\n",
                                frame.width, frame.height, frame.stride
                            ),
                        )?;
                        println!("  wrote {}", raw.display());
                    }
                    previous = Some(fingerprint);
                }
                Ok(None) => println!("  frame {attempt}: new=false (no new frame from WGC)"),
                Err(error) => {
                    println!(
                        "  frame {attempt}: error {error} (recoverable={})",
                        error.is_recoverable()
                    );
                    break;
                }
            }
        }
    }
    Ok(())
}

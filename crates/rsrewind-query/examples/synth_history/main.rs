//! Builds a throwaway data folder full of synthetic history for trying the UI and for the
//! website's screenshots: this machine plus two probes (exported and imported exactly as
//! `rsrewind export` / `import` do). Every screen is drawn here, procedurally, as an entirely
//! fictional desktop (see `scenes.rs`), and its "recognized text" is exactly the text drawn, at the
//! rectangles it was drawn in. Nothing here is real recorded history.
//!
//! ```text
//! cargo run -p rsrewind-query --example synth_history -- <empty folder> [moments per source] [end, Unix ms]
//! rsrewind --data-dir <folder> ui
//! ```
//!
//! The same arguments give the same pictures (seeded); without an end time the history ends now.
//! Fonts come from the operating system (Segoe UI and Cascadia Mono / Consolas on Windows; Noto or
//! DejaVu elsewhere; `SYNTH_FONT_DIR` to point elsewhere); none are bundled.
//!
//! Lives in `rsrewind-query` because it is the only crate that may use the writer
//! (`rsrewind-storage`) as a dev-dependency without the UI depending on it. It is an example,
//! never shipped code.

mod draw;
mod scenes;

use draw::{Fallible, Fonts};
use rsrewind_core::paths::media_relative_path;
use rsrewind_core::{ApplicationContext, DataDir, MonitorInfo, Timestamp, WindowContext};
use rsrewind_segment::Segment;
use rsrewind_storage::media::{encode_webp, write_webp_exclusive};
use rsrewind_storage::{
    ExportOptions, MIN_SETTLE_MS, NewVisualState, Observation, Store, import_into_root,
};
use scenes::{H, Persona, Rng, W};
use std::path::PathBuf;

/// One machine to fabricate.
struct Machine<'a> {
    host: &'a str,
    persona: Persona,
    seed: u64,
    monitors: usize,
}

/// Records `count` moments of `machine` ending near `end`.
fn record(
    data: &DataDir,
    fonts: &Fonts,
    machine: &Machine<'_>,
    count: usize,
    end: i64,
) -> Fallible<Store> {
    let store = Store::open(data)?;
    let mut rng = Rng(machine.seed);
    let session =
        store.begin_session(Timestamp(end - 7 * 86_400_000), machine.host, "synthetic")?;
    let mut ids = Vec::new();
    for m in 0..machine.monitors {
        ids.push(store.upsert_monitor(
            &MonitorInfo {
                device_name: format!(r"\\.\DISPLAY{}", m + 1),
                left: m as i32 * W as i32,
                top: 0,
                width: W,
                height: H,
                dpi: 96,
                primary: m == 0,
            },
            Timestamp(end),
        )?);
    }
    // Bursty: mostly seconds to minutes apart, sometimes an idle gap of an hour or more.
    let mut at = end;
    let mut moments = Vec::new();
    for _ in 0..count {
        let gap = match rng.below(20) {
            0 => 3_600_000 + rng.below(10_800_000) as i64,
            1..=4 => 120_000 + rng.below(900_000) as i64,
            _ => 4_000 + rng.below(50_000) as i64,
        };
        at -= gap;
        moments.push(at);
    }
    moments.reverse();
    for (i, at) in moments.iter().copied().enumerate() {
        let monitor = ids[rng.below(ids.len() as u64) as usize];
        let scene = scenes::draw(fonts, &mut rng, machine.persona, at);
        let (frame, blocks) = scene.canvas.into_frame();
        let bytes = encode_webp(&frame, 82)?;
        let relative = media_relative_path(Timestamp(at), monitor);
        write_webp_exclusive(data, &relative, &bytes)?;
        let state = store.insert_visual_state(&NewVisualState {
            monitor,
            captured_at: Timestamp(at),
            media_path: relative.clone(),
            width: W,
            height: H,
            byte_size: bytes.len() as u64,
            fingerprint: None,
            ocr_enabled: true,
        })?;
        store.save_ocr(state, &relative, &blocks, "synthetic", 1)?;
        let application = store.upsert_application(
            &ApplicationContext {
                process_name: scene.process.into(),
                exe_path: None,
            },
            Timestamp(at),
        )?;
        let window = store.upsert_window(
            application,
            &WindowContext {
                title: scene.title,
                class_name: None,
            },
            Timestamp(at),
        )?;
        let hold = 2_000 + rng.below(20_000) as i64;
        let next = moments.get(i + 1).copied().unwrap_or(end);
        for t in [at, (at + hold).min(next - 1).max(at)] {
            store.record_observation(&Observation {
                session,
                monitor,
                visual_state: state,
                application: Some(application),
                window: Some(window),
                at: Timestamp(t),
                max_gap_ms: 60_000,
            })?;
        }
    }
    Ok(store)
}

fn main() -> Fallible<()> {
    let mut args = std::env::args().skip(1);
    let root = PathBuf::from(
        args.next()
            .ok_or("usage: synth_history <empty folder> [moments per source] [end, Unix ms]")?,
    );
    let count: usize = args.next().map(|n| n.parse()).transpose()?.unwrap_or(120);
    let end: i64 = args
        .next()
        .map(|n| n.parse())
        .transpose()?
        .unwrap_or_else(|| Timestamp::now().0);
    if root.join("recall.db").exists() {
        return Err("that folder already holds history; give an empty or new folder".into());
    }
    let fonts = Fonts::load()?;
    let central = DataDir::new(&root);
    let ops = Machine {
        host: "central",
        persona: Persona::Ops,
        seed: 0x9E37_79B9_7F4A_7C15,
        monitors: 2,
    };
    record(&central, &fonts, &ops, count, end)?;
    println!("this machine: {count} moments");

    let scratch = root.join("probe-scratch");
    for probe in [
        Machine {
            host: "FRONT-DESK-PC",
            persona: Persona::FrontDesk,
            seed: 0xD1B5_4A32_D192_ED03,
            monitors: 1,
        },
        Machine {
            host: "kitchen-kiosk",
            persona: Persona::Kiosk,
            seed: 0x2545_F491_4F6C_DD1D,
            monitors: 1,
        },
    ] {
        let label = probe.host;
        let store = record(
            &DataDir::new(scratch.join(label)),
            &fonts,
            &probe,
            count,
            end - 30_000,
        )?;
        let outbox = scratch.join(format!("{label}-outbox"));
        let mut segments = 0;
        while let Some(report) = store.export_segment(
            &outbox,
            &ExportOptions {
                label,
                platform: "synthetic",
                app_version: env!("CARGO_PKG_VERSION"),
                capabilities: &["ocr", "window_titles", "process_names"],
                now: Timestamp(end + 3_600_000),
                settle_ms: MIN_SETTLE_MS,
                max_events: 500,
            },
        )? {
            import_into_root(&central, &Segment::open(&report.path)?)?;
            segments += 1;
        }
        println!("{label}: {count} moments in {segments} segment(s)");
    }
    std::fs::remove_dir_all(&scratch)?;
    println!("done: rsrewind --data-dir {} ui", root.display());
    Ok(())
}

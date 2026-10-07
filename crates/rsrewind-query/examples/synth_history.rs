//! Builds a throwaway data folder full of synthetic history for trying the UI: this machine plus
//! two probes (exported and imported exactly as `rsrewind export`/`import` do), with drawn
//! "screens" and recognized text. Nothing here is real recorded history.
//!
//! ```text
//! cargo run -p rsrewind-query --example synth_history -- <empty folder> [moments per source]
//! rsrewind --data-dir <folder> ui
//! ```
//!
//! Lives in `rsrewind-query` because it is the only crate that may use the writer
//! (`rsrewind-storage`) as a dev-dependency without the UI depending on it.

use rsrewind_core::paths::media_relative_path;
use rsrewind_core::{
    ApplicationContext, BgraFrame, DataDir, MonitorInfo, OcrBlock, Timestamp, WindowContext,
};
use rsrewind_segment::Segment;
use rsrewind_storage::media::{encode_webp, write_webp_exclusive};
use rsrewind_storage::{
    ExportOptions, MIN_SETTLE_MS, NewVisualState, Observation, Store, import_into_root,
};
use std::path::PathBuf;

type Fallible<T> = Result<T, Box<dyn std::error::Error>>;

const W: u32 = 960;
const H: u32 = 600;

struct App {
    process: &'static str,
    titles: &'static [&'static str],
    color: [u8; 3],
    phrases: &'static [&'static str],
}

const APPS: &[App] = &[
    App {
        process: "chrome.exe",
        titles: &[
            "Konica printer - Settings",
            "Supply order",
            "Ticket 8812 - Helpdesk",
        ],
        color: [66, 133, 244],
        phrases: &[
            "Konica bizhub C360 toner low",
            "order 48 cartridges",
            "ticket 8812 printer jam tray 2",
        ],
    },
    App {
        process: "EXCEL.EXE",
        titles: &["Quarterly budget.xlsx", "Invoice tracker.xlsx"],
        color: [33, 115, 70],
        phrases: &[
            "quarterly budget forecast",
            "invoice 4471 overdue",
            "toner spend Q3",
        ],
    },
    App {
        process: "Teams.exe",
        titles: &["Standup", "Facilities chat"],
        color: [98, 100, 167],
        phrases: &[
            "standup notes floor plan v3",
            "lunch order for friday",
            "printer is fixed",
        ],
    },
    App {
        process: "WindowsTerminal.exe",
        titles: &["pwsh", "Administrator: Windows PowerShell"],
        color: [40, 40, 48],
        phrases: &[
            "cargo test --workspace",
            "Get-Printer | Where Name -like konica",
        ],
    },
    App {
        process: "OUTLOOK.EXE",
        titles: &["Inbox", "Re: kitchen kiosk menu"],
        color: [0, 114, 198],
        phrases: &[
            "dinner menu tuesday",
            "re invoice 4471",
            "kiosk reboot schedule",
        ],
    },
];

/// A tiny deterministic generator (no extra dependency).
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

/// Draws an abstract application window: desktop, window, title bar, lines of "text".
fn screen(app: &App, rng: &mut Rng, desk: [u8; 3]) -> BgraFrame {
    let mut pixels = vec![0u8; (W * H * 4) as usize];
    let mut fill = |x0: u32, y0: u32, x1: u32, y1: u32, [r, g, b]: [u8; 3]| {
        for y in y0.min(H)..y1.min(H) {
            for x in x0.min(W)..x1.min(W) {
                let i = ((y * W + x) * 4) as usize;
                pixels[i..i + 4].copy_from_slice(&[b, g, r, 255]);
            }
        }
    };
    fill(0, 0, W, H, desk);
    let (wx, wy) = (30 + rng.below(80) as u32, 24 + rng.below(50) as u32);
    let (ww, wh) = (W - wx - 30 - rng.below(80) as u32, H - wy - 40);
    fill(wx, wy, wx + ww, wy + wh, [246, 244, 240]);
    fill(wx, wy, wx + ww, wy + 34, app.color);
    fill(wx + 12, wy + 50, wx + 180, wy + wh - 12, [232, 229, 224]);
    let lines = 8 + rng.below(10) as u32;
    for line in 0..lines {
        let y = wy + 60 + line * 26;
        let len = 120 + rng.below(u64::from(ww.saturating_sub(260))) as u32;
        fill(wx + 200, y, wx + 200 + len, y + 9, [70, 70, 78]);
    }
    let accent = [
        app.color[0].saturating_add(60),
        app.color[1].saturating_add(40),
        app.color[2],
    ];
    let bx = wx + 200 + rng.below(200) as u32;
    fill(bx, wy + wh - 120, bx + 160, wy + wh - 30, accent);
    BgraFrame {
        width: W,
        height: H,
        stride: W * 4,
        pixels,
    }
}

/// Records `count` moments ending near `end`, on one or two monitors.
fn record(
    data: &DataDir,
    host: &str,
    seed: u64,
    count: usize,
    end: i64,
    monitors: usize,
) -> Fallible<Store> {
    let store = Store::open(data)?;
    let mut rng = Rng(seed);
    let session = store.begin_session(Timestamp(end - 7 * 86_400_000), host, "synthetic")?;
    let mut ids = Vec::new();
    for m in 0..monitors {
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
    let desk = [
        20 + rng.below(30) as u8,
        24 + rng.below(30) as u8,
        34 + rng.below(30) as u8,
    ];
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
        let app = &APPS[rng.below(APPS.len() as u64) as usize];
        let title = app.titles[rng.below(app.titles.len() as u64) as usize];
        let monitor = ids[rng.below(ids.len() as u64) as usize];
        let frame = screen(app, &mut rng, desk);
        let bytes = encode_webp(&frame, 70)?;
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
        let blocks: Vec<OcrBlock> = (0..2 + rng.below(3) as u32)
            .map(|line| OcrBlock {
                text: app.phrases[rng.below(app.phrases.len() as u64) as usize].into(),
                x: 200.0,
                y: 60.0 + 26.0 * line as f32,
                width: 400.0,
                height: 12.0,
                confidence: None,
                line_index: line,
            })
            .collect();
        store.save_ocr(state, &relative, &blocks, "synthetic", 1)?;
        let application = store.upsert_application(
            &ApplicationContext {
                process_name: app.process.into(),
                exe_path: None,
            },
            Timestamp(at),
        )?;
        let window = store.upsert_window(
            application,
            &WindowContext {
                title: title.into(),
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
            .ok_or("usage: synth_history <empty folder> [moments]")?,
    );
    let count: usize = args.next().map(|n| n.parse()).transpose()?.unwrap_or(120);
    if root.join("recall.db").exists() {
        return Err("that folder already holds history; give an empty or new folder".into());
    }
    let now = Timestamp::now().0;
    let central = DataDir::new(&root);
    record(&central, "central", 0x9E37_79B9_7F4A_7C15, count, now, 2)?;
    println!("this machine: {count} moments");

    let scratch = root.join("probe-scratch");
    for (label, seed) in [
        ("FRONT-DESK-PC", 0xD1B5_4A32_D192_ED03),
        ("kitchen-kiosk", 0x2545_F491_4F6C_DD1D),
    ] {
        let probe = DataDir::new(scratch.join(label));
        let store = record(&probe, label, seed, count, now - 30_000, 1)?;
        let outbox = scratch.join(format!("{label}-outbox"));
        let mut segments = 0;
        while let Some(report) = store.export_segment(
            &outbox,
            &ExportOptions {
                label,
                platform: "synthetic",
                app_version: env!("CARGO_PKG_VERSION"),
                capabilities: &["ocr", "window_titles", "process_names"],
                now: Timestamp(now + 3_600_000),
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

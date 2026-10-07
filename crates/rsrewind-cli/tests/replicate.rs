//! Drives the real `rsrewind` binary through export -> import -> search, the way an operator would.

use rsrewind_core::paths::media_relative_path;
use rsrewind_core::{BgraFrame, DataDir, MonitorInfo, OcrBlock, Timestamp};
use rsrewind_storage::media::{encode_webp, write_webp_exclusive};
use rsrewind_storage::{NewVisualState, Observation, Store};
use std::process::{Command, Output};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn rsrewind(data: &std::path::Path, args: &[&str]) -> Result<Output, Box<dyn std::error::Error>> {
    Ok(Command::new(env!("CARGO_BIN_EXE_rsrewind"))
        .arg("--data-dir")
        .arg(data)
        .args(args)
        .env_remove("RSREWIND_DATA_DIR")
        .output()?)
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn export_import_search_round_trip_through_the_binary() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let probe = tmp.path().join("probe");
    let central = tmp.path().join("central");

    // A probe with one moment recorded an hour ago.
    let at = Timestamp::now().saturating_sub_millis(3_600_000);
    {
        let data = DataDir::new(&probe);
        let store = Store::open(&data)?;
        let session = store.begin_session(at, "PROBE", "0.0.1")?;
        let monitor = store.upsert_monitor(
            &MonitorInfo {
                device_name: "D1".into(),
                left: 0,
                top: 0,
                width: 8,
                height: 8,
                dpi: 96,
                primary: true,
            },
            at,
        )?;
        let frame = BgraFrame {
            width: 8,
            height: 8,
            stride: 32,
            pixels: vec![200; 8 * 8 * 4],
        };
        let bytes = encode_webp(&frame, 80)?;
        let relative = media_relative_path(at, monitor);
        write_webp_exclusive(&data, &relative, &bytes)?;
        let state = store.insert_visual_state(&NewVisualState {
            monitor,
            captured_at: at,
            media_path: relative.clone(),
            width: 8,
            height: 8,
            byte_size: bytes.len() as u64,
            fingerprint: None,
            ocr_enabled: true,
        })?;
        store.save_ocr(
            state,
            &relative,
            &[OcrBlock {
                text: "zebra crossing schedule".into(),
                x: 0.0,
                y: 0.0,
                width: 1.0,
                height: 1.0,
                confidence: None,
                line_index: 0,
            }],
            "t",
            1,
        )?;
        store.record_observation(&Observation {
            session,
            monitor,
            visual_state: state,
            application: None,
            window: None,
            at,
            max_gap_ms: 5_000,
        })?;
    }

    let exported = rsrewind(&probe, &["export", "--json"])?;
    assert!(exported.status.success(), "{}", text(&exported));
    let rows: serde_json::Value = serde_json::from_slice(&exported.stdout)?;
    assert_eq!(rows.as_array().map(Vec::len), Some(1));
    let segment = rows[0]["file"].as_str().ok_or("no file in export output")?;

    // Nothing new: a second export seals nothing.
    let again = rsrewind(&probe, &["export", "--json"])?;
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&again.stdout)?,
        serde_json::json!([])
    );

    // Import, twice; the second is harmless.
    for expect in ["imported", "already imported"] {
        let out = rsrewind(&central, &["import", segment])?;
        assert!(out.status.success(), "{}", text(&out));
        assert!(text(&out).contains(expect), "{}", text(&out));
    }
    // A truncated copy is refused and reported, with a failing exit code.
    let bytes = std::fs::read(segment)?;
    let cut = tmp.path().join("cut.rsseg");
    std::fs::write(&cut, &bytes[..bytes.len() / 2])?;
    let refused = rsrewind(&central, &["import", cut.to_str().ok_or("path")?])?;
    assert!(!refused.status.success());
    assert!(text(&refused).contains("refused"), "{}", text(&refused));

    // The central instance lists the source and searches its replica with the ordinary command.
    let sources = rsrewind(&central, &["sources", "--json"])?;
    let listed: serde_json::Value = serde_json::from_slice(&sources.stdout)?;
    let replica = listed[0]["data_dir"].as_str().ok_or("no data_dir")?;
    assert_eq!(listed[0]["segments"], 1);
    let found = rsrewind(std::path::Path::new(replica), &["search", "zebra"])?;
    assert!(found.status.success(), "{}", text(&found));
    assert!(text(&found).contains("zebra"), "{}", text(&found));
    Ok(())
}

#[test]
fn export_refuses_a_settle_time_below_the_safety_minimum() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let out = rsrewind(tmp.path(), &["export", "--settle-minutes", "0"])?;
    assert!(!out.status.success());
    Ok(())
}

#[test]
fn forget_deletes_sealed_segments_for_that_time_and_says_what_it_cannot_reach() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let probe = tmp.path().join("probe");
    let at = Timestamp::now().saturating_sub_millis(3_600_000);
    {
        let data = DataDir::new(&probe);
        let store = Store::open(&data)?;
        let session = store.begin_session(at, "PROBE", "0.0.1")?;
        let monitor = store.upsert_monitor(
            &MonitorInfo {
                device_name: "D1".into(),
                left: 0,
                top: 0,
                width: 8,
                height: 8,
                dpi: 96,
                primary: true,
            },
            at,
        )?;
        let bytes = encode_webp(
            &BgraFrame {
                width: 8,
                height: 8,
                stride: 32,
                pixels: vec![9; 256],
            },
            80,
        )?;
        let relative = media_relative_path(at, monitor);
        write_webp_exclusive(&data, &relative, &bytes)?;
        let state = store.insert_visual_state(&NewVisualState {
            monitor,
            captured_at: at,
            media_path: relative,
            width: 8,
            height: 8,
            byte_size: bytes.len() as u64,
            fingerprint: None,
            ocr_enabled: false,
        })?;
        store.record_observation(&Observation {
            session,
            monitor,
            visual_state: state,
            application: None,
            window: None,
            at,
            max_gap_ms: 5_000,
        })?;
    }
    assert!(rsrewind(&probe, &["export"])?.status.success());
    let outbox = probe.join("outbox");
    assert_eq!(std::fs::read_dir(&outbox)?.count(), 1);

    let out = rsrewind(&probe, &["forget", "3h", "--yes"])?;
    assert!(out.status.success(), "{}", text(&out));
    let said = text(&out);
    assert!(said.contains("sealed segment file"), "{said}");
    assert!(said.contains("already been sealed"), "{said}");
    assert_eq!(
        std::fs::read_dir(&outbox)?.count(),
        0,
        "segment survived a forget"
    );

    let doctor = rsrewind(&probe, &["doctor", "--json"])?;
    assert!(text(&doctor).contains("replication"), "{}", text(&doctor));
    Ok(())
}

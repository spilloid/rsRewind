use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const SOURCE: &str = "0123456789abcdef0123456789abcdef";

fn manifest() -> Manifest {
    Manifest {
        format: FORMAT_VERSION,
        source: SourceInfo {
            id: SOURCE.into(),
            label: "probe".into(),
            platform: "windows".into(),
            app_version: "0.0.1".into(),
            capabilities: vec!["ocr".into()],
        },
        seq: 1,
        created_at: 10,
        cursor: ExportCursor {
            cut_ms: 5,
            max_event_id: 2,
        },
        sessions: vec![SessionRec {
            started_at: 1,
            ended_at: None,
            hostname: "h".into(),
            app_version: "0.0.1".into(),
        }],
        monitors: vec![MonitorRec {
            device_name: "D1".into(),
            left: 0,
            top: 0,
            width: 10,
            height: 10,
            dpi: 96,
            primary: true,
        }],
        applications: vec![ApplicationRec {
            process_name: "a.exe".into(),
            exe_path: None,
        }],
        windows: vec![WindowRec {
            application: 0,
            title: "t".into(),
            class_name: None,
        }],
        states: vec![
            StateRec {
                monitor: 0,
                captured_at: 2,
                width: 10,
                height: 10,
                fingerprint: Some(u64::MAX),
                blob_len: 3,
                ocr_state: OcrState::Done,
                ocr_engine: Some("e".into()),
                ocr_ms: Some(4),
                ocr_blocks: vec![OcrRec {
                    text: "hello".into(),
                    x: 0.0,
                    y: 0.0,
                    width: 1.0,
                    height: 1.0,
                    confidence: None,
                    line_index: 0,
                }],
            },
            StateRec {
                monitor: 0,
                captured_at: 3,
                width: 10,
                height: 10,
                fingerprint: None,
                blob_len: 2,
                ocr_state: OcrState::Pending,
                ocr_engine: None,
                ocr_ms: None,
                ocr_blocks: vec![],
            },
        ],
        events: vec![EventRec {
            kind: "observation".into(),
            started_at: 2,
            ended_at: 4,
            session: 0,
            monitor: Some(0),
            state: Some(0),
            application: Some(0),
            window: Some(0),
            metadata_json: None,
        }],
    }
}

fn blobs(i: usize) -> std::io::Result<Vec<u8>> {
    Ok(if i == 0 { vec![1, 2, 3] } else { vec![9, 8] })
}

fn write(dir: &Path, m: &Manifest) -> Result<Written> {
    write_segment(&dir.join(file_name(&m.source.id, m.seq)), m, blobs)
}

#[test]
fn round_trips_manifest_and_blobs() -> TestResult {
    let dir = tempfile::tempdir()?;
    let written = write(dir.path(), &manifest())?;
    let segment = Segment::open(&written.path)?;
    assert_eq!(segment.manifest(), &manifest());
    assert_eq!(segment.content_hash(), written.content_hash);
    assert_eq!(segment.read_blob(0)?, vec![1, 2, 3]);
    assert_eq!(segment.read_blob(1)?, vec![9, 8]);
    assert!(segment.read_blob(2).is_err());
    assert_eq!(written.bytes, std::fs::metadata(&written.path)?.len());
    Ok(())
}

#[test]
fn identical_input_gives_identical_bytes() -> TestResult {
    let a = tempfile::tempdir()?;
    let b = tempfile::tempdir()?;
    assert_eq!(
        write(a.path(), &manifest())?.content_hash,
        write(b.path(), &manifest())?.content_hash
    );
    Ok(())
}

#[test]
fn a_truncated_file_never_verifies() -> TestResult {
    let dir = tempfile::tempdir()?;
    let written = write(dir.path(), &manifest())?;
    let full = std::fs::read(&written.path)?;
    // Every proper prefix is a "partial upload" and must be refused.
    for cut in [
        0,
        7,
        15,
        16,
        full.len() / 2,
        full.len() - 41,
        full.len() - 1,
    ] {
        let partial = dir.path().join("partial.rsseg");
        std::fs::write(&partial, &full[..cut])?;
        assert!(
            Segment::open(&partial).is_err(),
            "prefix of {cut} bytes verified"
        );
    }
    Ok(())
}

#[test]
fn flipping_any_region_is_detected() -> TestResult {
    let dir = tempfile::tempdir()?;
    let written = write(dir.path(), &manifest())?;
    let full = std::fs::read(&written.path)?;
    let manifest_len = u64::from_le_bytes(full[8..16].try_into()?) as usize;
    for at in [
        20,
        16 + manifest_len,
        16 + manifest_len + 4,
        full.len() - 20,
    ] {
        let mut bad = full.clone();
        bad[at] ^= 0x01;
        let path = dir.path().join("bad.rsseg");
        std::fs::write(&path, &bad)?;
        assert!(
            matches!(
                Segment::open(&path),
                Err(SegmentError::DigestMismatch(_) | SegmentError::Manifest(_))
            ),
            "flip at {at} was not detected"
        );
    }
    Ok(())
}

#[test]
fn wrong_magic_is_not_a_segment() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("x.rsseg");
    std::fs::write(&path, vec![0u8; 200])?;
    assert!(matches!(
        Segment::open(&path),
        Err(SegmentError::BadMagic(_))
    ));
    Ok(())
}

#[test]
fn a_huge_declared_manifest_is_refused_not_allocated() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("x.rsseg");
    let mut bytes = MAGIC.to_vec();
    bytes.extend_from_slice(&(u64::MAX / 2).to_le_bytes());
    bytes.extend_from_slice(&[0u8; 64]);
    std::fs::write(&path, bytes)?;
    assert!(matches!(
        Segment::open(&path),
        Err(SegmentError::Truncated(_))
    ));
    Ok(())
}

#[test]
fn validation_rejects_bad_references_and_ids() {
    let mut m = manifest();
    m.events[0].state = Some(9);
    assert!(m.validate().is_err());

    let mut m = manifest();
    m.windows[0].application = 1;
    assert!(m.validate().is_err());

    let mut m = manifest();
    m.events[0].ended_at = 1;
    assert!(m.validate().is_err());

    let mut m = manifest();
    m.source.id = "../../etc/passwd".into();
    assert!(m.validate().is_err());

    let mut m = manifest();
    m.seq = 0;
    assert!(m.validate().is_err());

    let mut m = manifest();
    m.states[1].ocr_blocks = manifest().states[0].ocr_blocks.clone();
    assert!(m.validate().is_err());

    let mut m = manifest();
    m.format = FORMAT_VERSION + 1;
    assert!(matches!(
        m.validate(),
        Err(SegmentError::UnsupportedVersion { .. })
    ));
}

#[test]
fn source_ids_are_path_safe() {
    assert!(is_valid_source_id(SOURCE));
    for bad in [
        "",
        "ABCDEF0123456789ABCDEF0123456789",
        "0123456789abcdef0123456789abcde",
        "0123456789abcdef0123456789abcdeg",
        "..",
    ] {
        assert!(!is_valid_source_id(bad), "{bad:?}");
    }
}

#[test]
fn writing_never_replaces_an_existing_segment() -> TestResult {
    let dir = tempfile::tempdir()?;
    write(dir.path(), &manifest())?;
    assert!(matches!(
        write(dir.path(), &manifest()),
        Err(SegmentError::Exists(_))
    ));
    Ok(())
}

#[test]
fn a_blob_of_the_wrong_length_aborts_and_leaves_nothing() -> TestResult {
    let dir = tempfile::tempdir()?;
    let target = dir.path().join("seg.rsseg");
    let result = write_segment(&target, &manifest(), |_| Ok(vec![0u8; 7]));
    assert!(matches!(result, Err(SegmentError::BlobLength { .. })));
    assert!(!target.exists());
    assert_eq!(
        std::fs::read_dir(dir.path())?.count(),
        0,
        "temp file leaked"
    );
    Ok(())
}

#[test]
fn file_names_sort_and_parse_by_sequence() {
    assert_eq!(
        file_name(SOURCE, 7),
        "seg-01234567-0000000007.rsseg".to_string()
    );
    assert_eq!(seq_from_file_name(&file_name(SOURCE, 42)), Some(42));
    assert_eq!(seq_from_file_name("nope.txt"), None);
    assert!(file_name(SOURCE, 9) < file_name(SOURCE, 10));
}

//! Marks (schema v2): the migration from v1, writes, validation, and how marks follow the history
//! they point at through `forget` and retention (docs/design/marks-keyframes.md).

use super::*;
use crate::{MAX_MARK_LABEL_CHARS, MarkId};

fn marks(store: &Store) -> rusqlite::Result<Vec<(i64, i64, String)>> {
    let mut stmt = store
        .conn()
        .prepare("SELECT id, at, label FROM marks ORDER BY at, id")?;
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect()
}

fn has_table(conn: &Connection, name: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        [name],
        |r| r.get(0),
    )
}

#[test]
fn a_v1_store_with_history_is_backed_up_then_gains_marks() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let data = DataDir::new(tmp.path().join("rsRewind"));
    data.ensure()?;
    {
        // What a 0.0.x build left behind: schema v1 with a recorded session.
        let (v1, outcome) = Store::open_with_migrations(&data, &MIGRATIONS[..1], true)?;
        assert_eq!((outcome.from, outcome.to), (0, 1));
        v1.begin_session(Timestamp(T0), "host", "0.0.10")?;
        assert!(!has_table(v1.conn(), "marks")?);
    }

    let (store, outcome) = Store::open_with_migrations(&data, MIGRATIONS, false)?;
    assert_eq!((outcome.from, outcome.to), (1, 2));
    assert_eq!(store.schema_version()?, 2);
    assert!(has_table(store.conn(), "marks")?);
    let backup = outcome.backup.ok_or("no backup before the v2 migration")?;
    let name = backup.file_name().and_then(|n| n.to_str()).unwrap_or("");
    assert!(name.starts_with("recall-v1-"), "{name}");
    let copy = Connection::open(&backup)?;
    assert!(!has_table(&copy, "marks")?, "the backup is the v1 database");
    let sessions: i64 = copy.query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))?;
    assert_eq!(sessions, 1);
    drop(copy);

    // History survived, and the new table works.
    let kept: i64 = store
        .conn()
        .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))?;
    assert_eq!(kept, 1);
    store.add_mark(Timestamp(T0 + 5), "after the upgrade", Timestamp(T0 + 6))?;
    drop(store);

    // Idempotent: opening again migrates nothing and makes no second backup.
    for _ in 0..2 {
        let (again, outcome) = Store::open_with_migrations(&data, MIGRATIONS, false)?;
        assert_eq!((outcome.from, outcome.to, outcome.backup), (2, 2, None));
        assert_eq!(marks(&again)?.len(), 1);
    }
    let db_backups = std::fs::read_dir(data.backups())?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "db"))
        .count();
    assert_eq!(db_backups, 1);

    // A v1 build refuses the v2 database instead of guessing.
    assert!(matches!(
        Store::open_with_migrations(&data, &MIGRATIONS[..1], false),
        Err(StorageError::SchemaTooNew {
            found: 2,
            supported: 1
        })
    ));
    Ok(())
}

#[test]
fn marks_are_added_renamed_and_removed() -> TestResult {
    let f = fixture()?;
    let a = f
        .store
        .add_mark(Timestamp(T0 + 10), "  standup  ", Timestamp(T0))?;
    let b = f.store.add_mark(Timestamp(T0 + 5), "", Timestamp(T0))?;
    assert!(b.0 > a.0);
    assert_eq!(
        marks(&f.store)?,
        vec![
            (b.0, T0 + 5, String::new()),
            (a.0, T0 + 10, "standup".into())
        ]
    );

    assert!(f.store.rename_mark(b, "invoice 8812", Timestamp(T0 + 1))?);
    let updated: i64 =
        f.store
            .conn()
            .query_row("SELECT updated_at FROM marks WHERE id = ?1", [b.0], |r| {
                r.get(0)
            })?;
    assert_eq!(updated, T0 + 1);
    assert!(!f.store.rename_mark(MarkId(9_999), "x", Timestamp(T0))?);

    assert!(f.store.remove_mark(a)?);
    assert!(!f.store.remove_mark(a)?, "already gone");
    assert_eq!(marks(&f.store)?, vec![(b.0, T0 + 5, "invoice 8812".into())]);
    // Ids are never reused.
    let c = f.store.add_mark(Timestamp(T0 + 20), "", Timestamp(T0))?;
    assert!(c.0 > a.0);
    Ok(())
}

#[test]
fn labels_are_one_short_line() -> TestResult {
    let f = fixture()?;
    let at = Timestamp(T0);
    for bad in ["two\nlines", "tab\there", "bell\u{7}", "esc\u{1b}[31m"] {
        assert!(
            matches!(
                f.store.add_mark(at, bad, at),
                Err(StorageError::InvalidArgument(_))
            ),
            "{bad:?}"
        );
    }
    let longest = "é".repeat(MAX_MARK_LABEL_CHARS);
    let id = f.store.add_mark(at, &longest, at)?;
    let too_long = "é".repeat(MAX_MARK_LABEL_CHARS + 1);
    assert!(f.store.add_mark(at, &too_long, at).is_err());
    assert!(f.store.rename_mark(id, &too_long, at).is_err());
    assert!(f.store.rename_mark(id, "a\nb", at).is_err());
    // The table enforces the length even for a writer that skips `Store`.
    let raw = f.store.conn().execute(
        "INSERT INTO marks (at, label, created_at, updated_at) VALUES (1, ?1, 1, 1)",
        [&too_long],
    );
    assert!(raw.is_err());
    assert_eq!(marks(&f.store)?.len(), 1);
    Ok(())
}

#[test]
fn a_mark_cannot_be_put_inside_forgotten_time() -> TestResult {
    let f = fixture()?;
    f.store
        .delete_range(Timestamp(T0 + 1_000), Timestamp(T0 + 2_000))?;
    assert!(matches!(
        f.store
            .add_mark(Timestamp(T0 + 1_500), "", Timestamp(T0 + 3_000)),
        Err(StorageError::Fenced { .. })
    ));
    // The fence is half-open: its end is outside it.
    f.store
        .add_mark(Timestamp(T0 + 2_000), "", Timestamp(T0 + 3_000))?;
    f.store
        .add_mark(Timestamp(T0 + 999), "", Timestamp(T0 + 3_000))?;
    assert_eq!(marks(&f.store)?.len(), 2);
    Ok(())
}

#[test]
fn forget_deletes_the_marks_in_its_range_only() -> TestResult {
    let f = fixture()?;
    let now = Timestamp(T0 + 100_000);
    for (at, label) in [
        (T0, "before"),
        (T0 + 10_000, "inside"),
        (T0 + 19_999, "edge"),
    ] {
        f.store.add_mark(Timestamp(at), label, now)?;
    }
    f.store.add_mark(Timestamp(T0 + 20_000), "after", now)?;
    let report = f
        .store
        .delete_range(Timestamp(T0 + 1), Timestamp(T0 + 20_000))?;
    assert_eq!(report.marks_deleted, 2);
    let left: Vec<String> = marks(&f.store)?.into_iter().map(|m| m.2).collect();
    assert_eq!(left, ["before", "after"]);
    Ok(())
}

#[test]
fn age_retention_deletes_old_marks_and_size_retention_keeps_them() -> TestResult {
    let f = fixture()?;
    let day = 86_400_000;
    let now = T0 + 10 * day;
    // Pictures, so size retention has something to trim.
    for i in 0..4u8 {
        let at = now - 2 * day + i64::from(i) * 60_000;
        let vs = f.persist(f.monitor, at, i * 50)?;
        f.observe(f.monitor, vs, None, at)?;
    }
    f.store.add_mark(Timestamp(T0), "old", Timestamp(now))?;
    f.store
        .add_mark(Timestamp(now - 2 * day), "recent", Timestamp(now))?;

    let aged = f.store.apply_retention(7, 0, Timestamp(now))?;
    assert_eq!(aged.marks_deleted, 1);
    let left: Vec<String> = marks(&f.store)?.into_iter().map(|m| m.2).collect();
    assert_eq!(left, ["recent"]);

    // A cap far below the footprint: every picture goes, the mark stays.
    let sized = f.store.apply_retention(0, 1, Timestamp(now))?;
    assert!(sized.visual_states_deleted > 0, "{sized:?}");
    assert_eq!(sized.marks_deleted, 0);
    assert_eq!(marks(&f.store)?.len(), 1);
    Ok(())
}

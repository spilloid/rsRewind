//! Its own test binary: `tracing` caches whether anyone listens per process, so this runs alone.

mod common;

use common::*;
use serde_json::json;

#[test]
fn the_call_log_carries_no_screen_content() -> Fallible<()> {
    use std::sync::{Arc, Mutex};
    let (_dir, data) = history()?;
    let s = server(&data, enabled());
    let buffer = Arc::new(Mutex::new(Vec::<u8>::new()));
    let writer = buffer.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || Writer(writer.clone()))
        .with_ansi(false)
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        // Other tests' threads may have cached "nobody listens" for this callsite already.
        tracing::callsite::rebuild_interest_cache();
        let _ = call(&s, "search", json!({ "query": "linker" }));
        let _ = call(
            &s,
            "get_moment",
            json!({ "moment": "this:3", "include_lines": true }),
        );
    });
    let log = String::from_utf8(buffer.lock().map_err(|_| "lock")?.clone())?;
    assert!(
        log.contains("mcp call") && log.contains("tool=\"search\""),
        "{log}"
    );
    for secret in ["linker", "exit code", "konsole", "build", "Blue"] {
        assert!(!log.contains(secret), "log leaked '{secret}': {log}");
    }
    return Ok(());

    struct Writer(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Writer {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .map_err(|_| std::io::Error::other("lock"))?
                .extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
}

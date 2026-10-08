//! Recorder tests on deterministic fakes. They run on every platform: no Windows API, no real
//! clock, no sleeps. `capture_loop` drives single ticks against a queue the test holds;
//! `run` drives the whole recorder (persist and OCR threads, real SQLite) end to end.

mod capture_loop;
mod fakes;
mod run;

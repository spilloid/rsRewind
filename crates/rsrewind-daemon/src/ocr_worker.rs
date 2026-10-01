//! The OCR thread. Its queue is the database (`ocr_status = 'pending'`), so a backlog costs disk,
//! not memory, and survives restarts. The channel from the persist thread is only a doorbell.

use crate::counters::{self, Counters};
use rsrewind_ocr::OcrEngine;
use rsrewind_storage::{OcrStatus, StorageError, Store, media};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::Duration;

const BATCH: u32 = 8;
const IDLE_POLL: Duration = Duration::from_secs(5);

pub fn run(
    store: Store,
    language: Option<String>,
    wake: Receiver<()>,
    stop: Arc<AtomicBool>,
    counters: Arc<Counters>,
) {
    crate::win::lower_current_thread_priority();
    let engine = match OcrEngine::new(language.as_deref()) {
        Ok(engine) => engine,
        Err(error) => {
            // States stay `pending`; `rsrewind doctor` explains how to install a language.
            tracing::error!(%error, "OCR unavailable; captured states will wait for OCR");
            return;
        }
    };
    tracing::info!("OCR worker started");

    while !stop.load(Ordering::Relaxed) {
        let batch = match store.next_pending_ocr(BATCH) {
            Ok(batch) => batch,
            Err(error) => {
                tracing::warn!(%error, "could not read OCR backlog");
                Vec::new()
            }
        };
        if batch.is_empty() {
            match wake.recv_timeout(IDLE_POLL) {
                Ok(()) | Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => {
                    // Persist thread is gone: drain what is left, then exit.
                    if store.next_pending_ocr(1).map_or(true, |b| b.is_empty()) {
                        break;
                    }
                    continue;
                }
            }
        }
        for item in batch {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            let outcome = std::fs::read(&item.media_path)
                .map_err(|e| format!("read image: {e}"))
                .and_then(|bytes| media::decode_webp(&bytes).map_err(|e| e.to_string()))
                .and_then(|frame| engine.recognize(&frame).map_err(|e| e.to_string()));
            match outcome {
                Ok(output) => {
                    match store.save_ocr(
                        item.id,
                        &output.blocks,
                        "windows.media.ocr",
                        output.elapsed_ms,
                    ) {
                        Ok(()) => {
                            counters::bump(&counters.ocr_done);
                            counters::add(&counters.ocr_ms_total, output.elapsed_ms);
                            tracing::debug!(id = %item.id, lines = output.blocks.len(), ms = output.elapsed_ms, "ocr done");
                        }
                        // Deleted while we were reading it: nothing to index.
                        Err(StorageError::VisualStateMissing(_)) => {}
                        Err(error) => tracing::warn!(id = %item.id, %error, "could not save OCR"),
                    }
                }
                Err(message) => {
                    counters::bump(&counters.ocr_failed);
                    // The message names the failure (I/O, decode, WinRT HRESULT), never the text.
                    tracing::warn!(id = %item.id, error = %message, "OCR failed");
                    if let Err(error) = store.mark_ocr(item.id, OcrStatus::Failed, Some(&message)) {
                        tracing::warn!(id = %item.id, %error, "could not mark OCR failure");
                    }
                }
            }
        }
    }
    tracing::info!("OCR worker stopped");
}

//! Background work, so the UI thread never waits on SQLite or image decoding.
//!
//! - One thread owns the [`History`] facade (SQLite connections are not `Sync`) and answers
//!   [`Ask`]s in order. When questions of the same kind pile up (typing a search, scrubbing), only
//!   the newest is answered; the rest get [`Answer::Superseded`].
//! - Two threads decode pictures through the facade's [`MediaReader`] (no database), newest
//!   request first, with a bounded queue: while scrubbing, what is on screen now matters more than
//!   what was a second ago.
//!
//! Replies travel back as futures (`oneshot` channels) that iced awaits in a `Task`. Nothing here
//! logs what was on screen: only counts and timings, at debug level.

use crate::thumb::{self, Bgra};
use crate::timeline::FrameKey;
use iced::futures::channel::oneshot;
use rsrewind_core::{
    DataDir, SearchHit, SearchQuery, SourceId, TimelineCursor, TimelineEntry, Timestamp,
    VisualDetail, VisualStateId,
};
use rsrewind_query::{History, MediaReader, SourceFilter, SourceInfo, SourceProblem};
use std::collections::VecDeque;
use std::future::Future;
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

/// Decoded pictures waiting at most; older requests are dropped first.
const MAX_QUEUED_FRAMES: usize = 192;
const DECODERS: usize = 2;

/// A question for the history thread.
#[derive(Debug, Clone)]
pub enum Ask {
    Sources,
    /// Up to `limit` moments, newest first, at or before `at` (or the newest overall).
    Window {
        filter: SourceFilter,
        at: Option<Timestamp>,
        limit: u32,
    },
    Search {
        query: SearchQuery,
        filter: SourceFilter,
    },
    At {
        at: Timestamp,
        filter: SourceFilter,
    },
    Detail {
        source: Option<SourceId>,
        id: VisualStateId,
    },
}

impl Ask {
    /// Asks of one kind replace each other while queued; `Sources` never is replaced.
    fn kind(&self) -> u8 {
        match self {
            Self::Sources => 0,
            Self::Window { .. } => 1,
            Self::Search { .. } => 2,
            Self::At { .. } => 3,
            Self::Detail { .. } => 4,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Answer {
    Sources {
        sources: Vec<SourceInfo>,
        problems: Vec<SourceProblem>,
    },
    Window(Vec<TimelineEntry>),
    Search(Vec<SearchHit>),
    At(Option<TimelineEntry>),
    Detail(Option<VisualDetail>),
    /// A newer question of the same kind arrived first.
    Superseded,
    /// The query failed; the message is for display (no screen content in it).
    Failed(String),
    /// The worker is gone (only at shutdown).
    Gone,
}

struct Job {
    ask: Ask,
    reply: oneshot::Sender<Answer>,
}

/// Handle to the history thread. Cheap to clone.
#[derive(Clone)]
pub struct HistoryWorker {
    jobs: mpsc::Sender<Job>,
}

impl std::fmt::Debug for HistoryWorker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HistoryWorker")
    }
}

impl HistoryWorker {
    /// Starts the history thread (which opens the data folder) and the decoders it feeds.
    pub fn start(data: DataDir) -> std::io::Result<(Self, FramePool)> {
        let (jobs, inbox) = mpsc::channel::<Job>();
        let pool = FramePool::new();
        let decoders = pool.clone();
        std::thread::Builder::new()
            .name("rsrewind-ui-history".into())
            .spawn(move || {
                let started = std::time::Instant::now();
                let history = History::open(&data);
                tracing::debug!(
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    "history ready"
                );
                decoders.spawn_decoders(history.media());
                serve(&history, &inbox);
            })?;
        Ok((Self { jobs }, pool))
    }

    /// Sends a question; the future resolves with the answer.
    pub fn ask(&self, ask: Ask) -> impl Future<Output = Answer> + Send + 'static {
        let (reply, answer) = oneshot::channel();
        let sent = self.jobs.send(Job { ask, reply }).is_ok();
        async move {
            if !sent {
                return Answer::Gone;
            }
            answer.await.unwrap_or(Answer::Gone)
        }
    }
}

fn serve(history: &History, inbox: &mpsc::Receiver<Job>) {
    while let Ok(first) = inbox.recv() {
        let mut batch = vec![first];
        batch.extend(inbox.try_iter());
        for job in coalesce(batch) {
            let started = std::time::Instant::now();
            let kind = job.ask.kind();
            let answer = answer(history, job.ask);
            tracing::debug!(
                kind,
                elapsed_ms = started.elapsed().as_millis() as u64,
                "answered"
            );
            // The asker may have stopped waiting; that is fine.
            let _ = job.reply.send(answer);
        }
    }
}

/// Keeps the newest job of each replaceable kind, in arrival order; tells the others they were
/// superseded.
fn coalesce(batch: Vec<Job>) -> Vec<Job> {
    let newest: Vec<usize> = (0..batch.len())
        .filter(|&i| {
            let kind = batch[i].ask.kind();
            kind == 0 || !batch[i + 1..].iter().any(|j| j.ask.kind() == kind)
        })
        .collect();
    let mut keep = Vec::new();
    for (i, job) in batch.into_iter().enumerate() {
        if newest.contains(&i) {
            keep.push(job);
        } else {
            let _ = job.reply.send(Answer::Superseded);
        }
    }
    keep
}

fn answer(history: &History, ask: Ask) -> Answer {
    let failed = |e: rsrewind_query::QueryError| Answer::Failed(e.to_string());
    match ask {
        Ask::Sources => match history.sources() {
            Ok(sources) => Answer::Sources {
                sources,
                problems: history.problems().to_vec(),
            },
            Err(e) => failed(e),
        },
        Ask::Window { filter, at, limit } => history
            .recent(filter, limit, at.map(TimelineCursor::at_or_before))
            .map_or_else(failed, Answer::Window),
        Ask::Search { query, filter } => history
            .search(&query, filter)
            .map_or_else(failed, Answer::Search),
        Ask::At { at, filter } => history.at(at, filter).map_or_else(failed, Answer::At),
        Ask::Detail { source, id } => history
            .visual_detail(source, id)
            .map_or_else(failed, Answer::Detail),
    }
}

/// A picture to decode and shrink to fit `max_w` x `max_h`.
pub struct FrameJob {
    pub key: FrameKey,
    pub media_path: String,
    pub max_w: u32,
    pub max_h: u32,
    reply: oneshot::Sender<FrameAnswer>,
}

#[derive(Debug, Clone)]
pub enum FrameAnswer {
    Ready(Arc<Bgra>),
    Failed(String),
    /// Dropped from the queue (too many newer requests) or the pool is gone; ask again if needed.
    Dropped,
}

#[derive(Default)]
struct Queue {
    jobs: VecDeque<FrameJob>,
}

/// The decoder threads' shared queue. Cheap to clone.
#[derive(Clone)]
pub struct FramePool {
    shared: Arc<(Mutex<Queue>, Condvar)>,
}

impl std::fmt::Debug for FramePool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FramePool")
    }
}

fn lock(mutex: &Mutex<Queue>) -> MutexGuard<'_, Queue> {
    // A decoder that panicked mid-push cannot leave the queue half-updated in a way that matters
    // (VecDeque operations are not interrupted), so keep going with the data as it is.
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl FramePool {
    fn new() -> Self {
        Self {
            shared: Arc::new((Mutex::new(Queue::default()), Condvar::new())),
        }
    }

    fn spawn_decoders(&self, media: MediaReader) {
        for n in 0..DECODERS {
            let pool = self.clone();
            let media = media.clone();
            let spawned = std::thread::Builder::new()
                .name(format!("rsrewind-ui-decode-{n}"))
                .spawn(move || pool.decode_forever(&media));
            if let Err(error) = spawned {
                tracing::warn!(%error, "could not start a frame decoder");
            }
        }
    }

    /// Queues a picture; the future resolves when it is decoded, fails, or is dropped.
    pub fn request(
        &self,
        key: FrameKey,
        media_path: String,
        max_w: u32,
        max_h: u32,
    ) -> impl Future<Output = FrameAnswer> + Send + 'static {
        let (reply, answer) = oneshot::channel();
        {
            let (queue, ready) = &*self.shared;
            let mut queue = lock(queue);
            queue.jobs.push_back(FrameJob {
                key,
                media_path,
                max_w,
                max_h,
                reply,
            });
            while queue.jobs.len() > MAX_QUEUED_FRAMES {
                if let Some(old) = queue.jobs.pop_front() {
                    let _ = old.reply.send(FrameAnswer::Dropped);
                }
            }
            ready.notify_one();
        }
        async move { answer.await.unwrap_or(FrameAnswer::Dropped) }
    }

    fn next(&self) -> FrameJob {
        let (queue, ready) = &*self.shared;
        let mut queue = lock(queue);
        loop {
            // Newest first: what the user is looking at now.
            if let Some(job) = queue.jobs.pop_back() {
                return job;
            }
            queue = ready
                .wait(queue)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    fn decode_forever(&self, media: &MediaReader) {
        loop {
            let job = self.next();
            if job.reply.is_canceled() {
                continue;
            }
            let answer = match media.frame(job.key.0, &job.media_path) {
                Ok(frame) => match thumb::downscale(
                    &frame.pixels,
                    frame.width,
                    frame.height,
                    frame.stride,
                    job.max_w,
                    job.max_h,
                ) {
                    Some(small) => FrameAnswer::Ready(Arc::new(small)),
                    None => FrameAnswer::Failed("the stored picture is malformed".into()),
                },
                Err(error) => FrameAnswer::Failed(error.to_string()),
            };
            let _ = job.reply.send(answer);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(ask: Ask) -> (Job, oneshot::Receiver<Answer>) {
        let (reply, rx) = oneshot::channel();
        (Job { ask, reply }, rx)
    }

    fn search(text: &str) -> Ask {
        Ask::Search {
            query: SearchQuery {
                text: text.into(),
                ..SearchQuery::default()
            },
            filter: SourceFilter::All,
        }
    }

    #[test]
    fn only_the_newest_question_of_each_kind_is_answered() {
        let (a, mut ra) = job(search("k"));
        let (b, _rb) = job(Ask::Sources);
        let (c, mut rc) = job(search("ko"));
        let (d, _rd) = job(Ask::Sources);
        let (e, _re) = job(search("kon"));
        let kept = coalesce(vec![a, b, c, d, e]);
        let kinds: Vec<u8> = kept.iter().map(|j| j.ask.kind()).collect();
        assert_eq!(
            kinds,
            [0, 0, 2],
            "sources are never dropped; one search survives"
        );
        assert!(matches!(&kept[2].ask, Ask::Search { query, .. } if query.text == "kon"));
        assert!(matches!(ra.try_recv(), Ok(Some(Answer::Superseded))));
        assert!(matches!(rc.try_recv(), Ok(Some(Answer::Superseded))));
    }

    #[test]
    fn the_frame_queue_is_bounded_and_newest_first() {
        let pool = FramePool::new();
        let mut answers = Vec::new();
        for i in 0..(MAX_QUEUED_FRAMES + 5) {
            answers.push(pool.request((None, VisualStateId(i as i64)), String::new(), 1, 1));
        }
        let (queue, _) = &*pool.shared;
        assert_eq!(lock(queue).jobs.len(), MAX_QUEUED_FRAMES);
        let newest = pool.next();
        assert_eq!(newest.key.1, VisualStateId((MAX_QUEUED_FRAMES + 4) as i64));
        drop(answers);
    }
}

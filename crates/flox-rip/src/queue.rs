//! The one-at-a-time job queue, as the Mac's `Queue`.
//!
//! A single worker task takes the first queued job, runs it through the pipeline and
//! records the outcome. A failed job is re-queued once with `"retrying: …"` (two attempts
//! in total). Cancelling the running job stops its ffmpeg, downloads, sniff and pending
//! sends; cancelling any other job removes it from the list.
//!
//! The library channel is not passed in: it is looked up by the settings'
//! `telegram_channel` title (and created when missing, as the Mac's `ensureChannel`
//! does) the first time a job uploads, then cached for as long as the title stays the
//! same. A changed title in Settings is picked up by the next job.

use std::path::PathBuf;
use std::sync::{Arc, Weak};

use flox_core::error::{Error, Result};
use flox_core::settings::SettingsStore;
use flox_core::sniff::Sniffer;
use flox_td::chats;
use flox_td::transport::TdTransport;
use parking_lot::Mutex;
use tokio::runtime::Handle;
use tokio::sync::{watch, Notify};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::job::{Job, JobState, JobView};
use crate::tools::ToolPaths;
use crate::{pipeline, split, temp};

/// Failed attempts after which a job stays failed.
const ATTEMPTS: u32 = 2;

/// Callbacks for platform side effects (keep-awake, toast, library refresh).
pub trait QueueHooks: Send + Sync {
    /// `true` when the worker starts on a batch, `false` when the queue has drained.
    fn busy_changed(&self, busy: bool);
    /// After `busy_changed(false)`: whether any job in the list is Failed.
    fn drained(&self, any_failed: bool);
}

/// Everything the queue needs.
pub struct QueueDeps {
    pub td: Arc<dyn TdTransport>,
    pub sniffer: Option<Arc<dyn Sniffer>>,
    pub tools: ToolPaths,
    pub temp_root: PathBuf,
    pub settings: SettingsStore,
    pub hooks: Arc<dyn QueueHooks>,
}

/// Knobs that are fixed in the app and changed by tests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QueueOptions {
    /// Upload part size; also the largest local file that is hard-linked rather than copied.
    pub part_size: u64,
}

impl Default for QueueOptions {
    fn default() -> Self {
        Self {
            part_size: split::PART_SIZE,
        }
    }
}

/// Mutable queue state, behind one lock.
#[derive(Default)]
struct Inner {
    views: Vec<JobView>,
    /// The job the worker is running and its cancel token.
    current: Option<(Uuid, CancellationToken)>,
    /// Whether the worker task has been spawned.
    worker: bool,
}

/// The queue.
pub struct Queue {
    pub(crate) deps: QueueDeps,
    pub(crate) options: QueueOptions,
    inner: Mutex<Inner>,
    views: watch::Sender<Vec<JobView>>,
    wake: Arc<Notify>,
    shutdown: CancellationToken,
    runtime: Option<Handle>,
    me: Weak<Queue>,
    /// `(channel title, chat id)` of the last lookup.
    channel: Mutex<Option<(String, i64)>>,
}

impl Queue {
    /// An idle queue with the app's options.
    ///
    /// The worker is spawned on the tokio runtime current at this call, or, when there is
    /// none, on the one current at the first [`Queue::add`].
    pub fn new(deps: QueueDeps) -> Arc<Queue> {
        Self::with_options(deps, QueueOptions::default())
    }

    /// [`Queue::new`] with explicit options.
    pub fn with_options(deps: QueueDeps, options: QueueOptions) -> Arc<Queue> {
        let (views, _rx) = watch::channel(Vec::new());
        Arc::new_cyclic(|me| Queue {
            deps,
            options,
            inner: Mutex::new(Inner::default()),
            views,
            wake: Arc::new(Notify::new()),
            shutdown: CancellationToken::new(),
            runtime: Handle::try_current().ok(),
            me: me.clone(),
            channel: Mutex::new(None),
        })
    }

    /// Appends jobs in order and starts the worker if it is idle.
    pub fn add(&self, jobs: Vec<Job>) {
        if jobs.is_empty() {
            return;
        }
        self.edit(|i| i.views.extend(jobs.into_iter().map(JobView::queued)));
        self.kick();
    }

    /// Stops the running job (it becomes Cancelled), or removes any other job.
    pub fn cancel(&self, id: Uuid) {
        self.edit(|i| {
            let running = i.current.as_ref().filter(|(cur, _)| *cur == id);
            if let Some((_, token)) = running {
                token.cancel();
                if let Some(v) = i.views.iter_mut().find(|v| v.job.id == id) {
                    v.state = JobState::Cancelled;
                    v.detail = "stopping".to_string();
                    v.progress = None;
                }
            } else {
                i.views.retain(|v| v.job.id != id);
            }
        });
    }

    /// Re-queues a failed or cancelled job with a fresh pair of attempts.
    pub fn retry(&self, id: Uuid) {
        let mut queued = false;
        self.edit(|i| {
            let running = i.current.as_ref().is_some_and(|(cur, _)| *cur == id);
            if let Some(v) = i.views.iter_mut().find(|v| v.job.id == id) {
                if !running && matches!(v.state, JobState::Failed(_) | JobState::Cancelled) {
                    v.state = JobState::Queued;
                    v.detail.clear();
                    v.progress = None;
                    v.attempt = 0;
                    queued = true;
                }
            }
        });
        if queued {
            self.kick();
        }
    }

    /// Drops a job from the list; a running one is cancelled first.
    pub fn remove(&self, id: Uuid) {
        self.edit(|i| {
            if let Some((cur, token)) = &i.current {
                if *cur == id {
                    token.cancel();
                }
            }
            i.views.retain(|v| v.job.id != id);
        });
    }

    /// Drops every Done and Cancelled job. Failed jobs stay so they can be retried,
    /// as on the Mac.
    pub fn clear_finished(&self) {
        self.edit(|i| {
            i.views
                .retain(|v| !matches!(v.state, JobState::Done | JobState::Cancelled))
        });
    }

    /// The job list, updated on every change.
    pub fn snapshot(&self) -> watch::Receiver<Vec<JobView>> {
        self.views.subscribe()
    }

    /// Applies `f` to the state and publishes the list.
    fn edit<R>(&self, f: impl FnOnce(&mut Inner) -> R) -> R {
        let mut inner = self.inner.lock();
        let r = f(&mut inner);
        self.views.send_replace(inner.views.clone());
        r
    }

    /// Changes one job's view unless it was cancelled or removed meanwhile.
    pub(crate) fn update(&self, id: Uuid, f: impl FnOnce(&mut JobView)) {
        let mut inner = self.inner.lock();
        let Some(v) = inner.views.iter_mut().find(|v| v.job.id == id) else {
            return;
        };
        if v.state == JobState::Cancelled {
            return;
        }
        f(v);
        self.views.send_replace(inner.views.clone());
    }

    /// Sets a job's state and progress.
    pub(crate) fn set_state(&self, id: Uuid, state: JobState, progress: Option<f32>) {
        self.update(id, |v| {
            v.state = state;
            v.progress = progress;
        });
    }

    /// Sets a job's detail line.
    pub(crate) fn set_detail(&self, id: Uuid, detail: impl Into<String>) {
        let detail = detail.into();
        self.update(id, |v| v.detail = detail);
    }

    /// Sets a job's progress.
    pub(crate) fn set_progress(&self, id: Uuid, progress: f32) {
        self.update(id, |v| v.progress = Some(progress.clamp(0.0, 1.0)));
    }

    /// The library channel's chat id, found or created by the configured title.
    pub(crate) async fn chat_id(&self) -> Result<i64> {
        let title = self.deps.settings.get().telegram_channel;
        let cached = self
            .channel
            .lock()
            .as_ref()
            .filter(|(t, _)| *t == title)
            .map(|(_, id)| *id);
        if let Some(id) = cached {
            return Ok(id);
        }
        let id = chats::find_or_create_channel(self.deps.td.as_ref(), &title).await?;
        *self.channel.lock() = Some((title, id));
        Ok(id)
    }

    /// Wakes the worker, spawning it on first use.
    fn kick(&self) {
        let spawn = {
            let mut inner = self.inner.lock();
            !std::mem::replace(&mut inner.worker, true)
        };
        if spawn {
            let handle = self.runtime.clone().or_else(|| Handle::try_current().ok());
            match handle {
                Some(h) => {
                    h.spawn(worker(
                        self.me.clone(),
                        Arc::clone(&self.wake),
                        self.shutdown.clone(),
                    ));
                }
                None => {
                    self.inner.lock().worker = false;
                    tracing::error!("queue: no tokio runtime to run jobs on");
                    return;
                }
            }
        }
        self.wake.notify_one();
    }

    /// Picks the first queued job and makes it current.
    fn take_next(&self) -> Option<(Job, CancellationToken)> {
        let mut inner = self.inner.lock();
        let job = inner
            .views
            .iter()
            .find(|v| v.state == JobState::Queued)?
            .job
            .clone();
        let token = self.shutdown.child_token();
        inner.current = Some((job.id, token.clone()));
        Some((job, token))
    }

    fn has_queued(&self) -> bool {
        self.inner
            .lock()
            .views
            .iter()
            .any(|v| v.state == JobState::Queued)
    }

    /// Records how a run ended, as the Mac's `work()` loop.
    fn finish(&self, id: Uuid, token: &CancellationToken, result: Result<()>) {
        self.edit(|i| {
            i.current = None;
            let Some(v) = i.views.iter_mut().find(|v| v.job.id == id) else {
                return;
            };
            v.progress = None;
            match result {
                Ok(()) => {
                    v.state = JobState::Done;
                    v.detail = "uploaded".to_string();
                }
                Err(_) if token.is_cancelled() || v.state == JobState::Cancelled => {
                    v.state = JobState::Cancelled;
                    v.detail.clear();
                }
                Err(Error::Cancelled) => {
                    v.state = JobState::Cancelled;
                    v.detail.clear();
                }
                Err(e) => {
                    v.attempt += 1;
                    if v.attempt < ATTEMPTS {
                        v.state = JobState::Queued;
                        v.detail = format!("retrying: {e}");
                    } else {
                        v.state = JobState::Failed(e.to_string());
                        v.detail.clear();
                    }
                }
            }
        });
    }

    /// Runs queued jobs until none is left.
    async fn drain(&self) {
        self.deps.hooks.busy_changed(true);
        while let Some((job, token)) = self.take_next() {
            let dir = temp::job_dir(&self.deps.temp_root, job.id);
            let result = pipeline::run(self, &job, &dir, &token).await;
            if let Err(e) = tokio::fs::remove_dir_all(&dir).await {
                if e.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!("queue: could not remove {}: {e}", dir.display());
                }
            }
            if let Err(e) = &result {
                tracing::warn!("queue: {} failed: {e}", job.label());
            }
            self.finish(job.id, &token, result);
        }
        let any_failed = self
            .inner
            .lock()
            .views
            .iter()
            .any(|v| matches!(v.state, JobState::Failed(_)));
        self.deps.hooks.busy_changed(false);
        self.deps.hooks.drained(any_failed);
    }
}

impl Drop for Queue {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

/// The long-lived worker: sleeps until woken, then drains the queue.
async fn worker(me: Weak<Queue>, wake: Arc<Notify>, shutdown: CancellationToken) {
    loop {
        let pending = me.upgrade().is_some_and(|q| q.has_queued());
        if !pending {
            tokio::select! {
                () = shutdown.cancelled() => return,
                () = wake.notified() => continue,
            }
        }
        let Some(queue) = me.upgrade() else { return };
        queue.drain().await;
    }
}

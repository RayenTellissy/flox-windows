//! The one-at-a-time job queue. Filled in by piece P10.

use std::path::PathBuf;
use std::sync::Arc;

use flox_core::settings::SettingsStore;
use flox_core::sniff::Sniffer;
use flox_td::transport::TdTransport;
use tokio::sync::watch;
use uuid::Uuid;

use crate::job::{Job, JobView};
use crate::tools::ToolPaths;

/// Callbacks for platform side effects (keep-awake, toast, library refresh).
pub trait QueueHooks: Send + Sync {
    fn busy_changed(&self, busy: bool);
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

/// The queue.
pub struct Queue {
    #[allow(dead_code)]
    deps: QueueDeps,
    views: watch::Sender<Vec<JobView>>,
}

impl Queue {
    /// An idle queue. The worker task is added by P10.
    pub fn new(deps: QueueDeps) -> Arc<Queue> {
        let (views, _rx) = watch::channel(Vec::new());
        Arc::new(Queue { deps, views })
    }

    /// Appends jobs in order. Filled in by P10.
    #[allow(clippy::unimplemented)]
    pub fn add(&self, _jobs: Vec<Job>) {
        unimplemented!("flox_rip::queue::Queue::add (P10)")
    }

    /// Cancels a running job, or removes a queued one. Filled in by P10.
    #[allow(clippy::unimplemented)]
    pub fn cancel(&self, _id: Uuid) {
        unimplemented!("flox_rip::queue::Queue::cancel (P10)")
    }

    /// Re-queues a failed or cancelled job. Filled in by P10.
    #[allow(clippy::unimplemented)]
    pub fn retry(&self, _id: Uuid) {
        unimplemented!("flox_rip::queue::Queue::retry (P10)")
    }

    /// Drops a finished job from the list. Filled in by P10.
    #[allow(clippy::unimplemented)]
    pub fn remove(&self, _id: Uuid) {
        unimplemented!("flox_rip::queue::Queue::remove (P10)")
    }

    /// Drops every Done, Failed and Cancelled job. Filled in by P10.
    #[allow(clippy::unimplemented)]
    pub fn clear_finished(&self) {
        unimplemented!("flox_rip::queue::Queue::clear_finished (P10)")
    }

    /// The job list, updated on every change.
    pub fn snapshot(&self) -> watch::Receiver<Vec<JobView>> {
        self.views.subscribe()
    }
}

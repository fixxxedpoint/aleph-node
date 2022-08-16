use async_trait::async_trait;
use futures::channel::oneshot;
use log::{debug, trace, warn};

use crate::{
    network::{Data, DataNetwork, GuardedNetworkWrapper},
    party::{Handle, Task as PureTask},
    NodeIndex, SpawnHandle,
};

/// A wrapper for running the authority task within a specific session.
pub struct Task {
    task: PureTask,
    node_id: NodeIndex,
}

impl Task {
    /// Create a new authority task. The handle should be the handle to the actual task.
    pub fn new(handle: Handle, node_id: NodeIndex, exit: oneshot::Sender<()>) -> Self {
        Task {
            task: PureTask::new(handle, exit),
            node_id,
        }
    }

    /// Stop the authority task and wait for it to finish.
    pub async fn stop(self) {
        self.task.stop().await
    }

    /// If the authority task stops for any reason, this returns the associated NodeIndex, which
    /// can be used to restart the task.
    pub async fn stopped(&mut self) -> NodeIndex {
        self.task.stopped().await;
        self.node_id
    }
}

/// All the subtasks required to participate in a session as an authority.
pub struct Subtasks {
    exit: oneshot::Receiver<()>,
    member: PureTask,
    aggregator: PureTask,
    refresher: PureTask,
    data_store: PureTask,
}

impl Subtasks {
    /// Create the subtask collection by passing in all the tasks.
    pub fn new(
        exit: oneshot::Receiver<()>,
        member: PureTask,
        aggregator: PureTask,
        refresher: PureTask,
        data_store: PureTask,
    ) -> Self {
        Subtasks {
            exit,
            member,
            aggregator,
            refresher,
            data_store,
        }
    }

    async fn stop(self) {
        // both member and aggregator are implicitly using forwarder,
        // so we should force them to exit first to avoid any panics, i.e. `send on closed channel`
        debug!(target: "aleph-party", "Started to stop all tasks");
        self.member.stop().await;
        trace!(target: "aleph-party", "Member stopped");
        self.refresher.stop().await;
        trace!(target: "aleph-party", "Refresher stopped");
        self.data_store.stop().await;
        trace!(target: "aleph-party", "DataStore stopped");
        self.aggregator.stop().await;
        trace!(target: "aleph-party", "Aggregator stopped");
    }

    /// Blocks until the task is done and returns true if it quit unexpectedly.
    pub async fn failed(mut self) -> bool {
        let result = tokio::select! {
            _ = &mut self.exit => false,
            _ = self.member.stopped() => { warn!(target: "aleph-party", "Member stopped too early"); true },
            _ = self.aggregator.stopped() => { warn!(target: "aleph-party", "Aggregator stopped too early");true },
            _ = self.refresher.stopped() => { warn!(target: "aleph-party", "Refresher stopped too early"); true },
            _ = self.data_store.stopped() => { warn!(target: "aleph-party", "DataStore stopped too early"); true },
        };
        if result {
            debug!(target: "aleph-party", "Something died and it was unexpected");
        }
        self.stop().await;
        debug!(target: "aleph-party", "Stopped all processes");
        result
    }
}

pub struct HoldingSubtask<ST, D> {
    subtask: ST,
    _holder: D,
}

impl<ST, D> HoldingSubtask<ST, D> {
    pub fn new(subtask: ST, holder: D) -> Self {
        Self {
            subtask,
            _holder: holder,
        }
    }
}

#[async_trait]
pub trait FailingTask {
    async fn failed(mut self) -> bool;
}

#[async_trait]
impl FailingTask for Subtasks {
    async fn failed(mut self) -> bool {
        Subtasks::failed(self).await
    }
}

#[async_trait]
impl<ST: FailingTask + Send, D: Send> FailingTask for HoldingSubtask<ST, D> {
    async fn failed(mut self) -> bool {
        self.subtask.failed().await
    }
}

/// Common args for authority subtasks.
#[derive(Clone)]
pub struct SubtaskCommon {
    pub spawn_handle: SpawnHandle,
    pub session_id: u32,
}

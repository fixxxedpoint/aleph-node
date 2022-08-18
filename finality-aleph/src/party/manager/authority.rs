use futures::channel::oneshot;
use log::{debug, trace, warn};

use super::task::TaskStop;
use crate::{
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
    pub async fn stop(self) -> Result<TaskStop, ()> {
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
        let member_stop = self.member.stop().await;
        trace!(target: "aleph-party", "Member stopped");
        let aggregator_stop = self.aggregator.stop().await;
        trace!(target: "aleph-party", "Aggregator stopped");
        let refresher_stop = self.refresher.stop().await;
        trace!(target: "aleph-party", "Refresher stopped");
        let data_store_stop = self.data_store.stop().await;
        trace!(target: "aleph-party", "DataStore stopped");

        wait_stop_for_task(member_stop, "Member").await;
        wait_stop_for_task(aggregator_stop, "Aggregator").await;
        wait_stop_for_task(refresher_stop, "Refresher").await;
        wait_stop_for_task(data_store_stop, "DataStore").await;
    }

    /// Blocks until the task is done and returns true if it quit unexpectedly.
    pub async fn failed(mut self) -> bool {
        let result = tokio::select! {
            _ = &mut self.exit => false,
            _ = self.member.stopped() => { warn!(target: "aleph-party", "Member stopped too early"); true },
            _ = self.aggregator.stopped() => { warn!(target: "aleph-party", "Aggregator stopped too early");true },
            _ = self.refresher.stopped() => { warn!(target: "aleph-party", "Refresher stopped too early"); true },
            // TODO alternative solution: run data_store in this thread/task - do not spawn anything
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

async fn wait_stop_for_task(stop: Result<TaskStop, ()>, name: &'static str) {
    if let Ok(mut stop) = stop {
        stop.wait_stopped().await;
    } else if let Err(_) = stop {
        debug!(target: "aleph-party", "Task `{}` failed to stop", name);
    }
}

/// Common args for authority subtasks.
#[derive(Clone)]
pub struct SubtaskCommon {
    pub spawn_handle: SpawnHandle,
    pub session_id: u32,
}

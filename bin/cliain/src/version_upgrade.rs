use aleph_client::{ RootConnection, Sched };
use primitives::SessionIndex;

use crate::commands::Version;

pub fn schedule_upgrade(
    connection: RootConnection,
    version: Version,
    session_for_upgrade: SessionIndex,
) {
    connection.
}

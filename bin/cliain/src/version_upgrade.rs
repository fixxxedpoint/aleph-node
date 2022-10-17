use aleph_client::{RootConnection, VersionUpgrade};
use primitives::SessionIndex;

use crate::commands::Version;

pub fn schedule_upgrade(
    connection: RootConnection,
    version: Version,
    session_for_upgrade: SessionIndex,
) -> anyhow::Result<()> {
    connection
        .schedule_upgrade(version, session_for_upgrade)
        .unwrap_or_else(|err| {
            panic!("unable to schedule an upgrade {:?}", err);
        });
}

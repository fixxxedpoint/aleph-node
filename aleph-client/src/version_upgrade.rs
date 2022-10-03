use primitives::SessionIndex;
use substrate_api_client::XtStatus;

use crate::{try_send_xt, RootConnection, VersionUpgrade};

impl VersionUpgrade for RootConnection {
    type Version = u32;
    type Error = &str;

    fn schedule_upgrade(&self, version: Version, session: SessionIndex) -> Result<(), Self::Error> {
        let connection = self.as_connection();
        let upgrade_call = compose_call!(
            connection.metadata,
            "Aleph",
            "schedule_aleph_bft_version_change",
            version,
            session
        );
        let xt = compose_extrinsic!(connection, "Sudo", "sudo", upgrade_call);
        try_send_xt(
            &connection,
            xt,
            Some("schedule_aleph_bft_version_change"),
            XtStatus::Finalized,
        )
        .map(|_| ())
    }
}

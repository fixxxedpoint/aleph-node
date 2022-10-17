use primitives::SessionIndex;
use sp_core::Pair;
use substrate_api_client::{compose_call, compose_extrinsic, ExtrinsicParams, XtStatus};

use crate::{try_send_xt, AnyConnection, RootConnection, VersionUpgrade};

impl VersionUpgrade for RootConnection {
    type Version = u32;
    type Error = substrate_api_client::ApiClientError;

    fn schedule_upgrade(
        &self,
        version: Self::Version,
        session: SessionIndex,
    ) -> anyhow::Result<(), Self::Error> {
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

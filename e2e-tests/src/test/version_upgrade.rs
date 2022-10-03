use aleph_client::{
    get_current_block_number, get_session_first_block, get_session_period,
    wait_for_at_least_session, wait_for_finalized_block, AnyConnection, VersionUpgrade,
};

use crate::{config::VersionUpgradeParams, Config};

const DEFAULT_UPGRADE_OPTIONS: VersionUpgradeParams = VersionUpgradeParams {
    version: 1,
    session: 3,
    finalization_wait_sessions: 3,
};

// Simple test that schedules a version upgrade, awaits it, and checks if node is still finalizing after planned upgrade session.
pub fn schedule_version_change(config: &Config) -> anyhow::Result<()> {
    let connection = config.create_root_connection();
    let upgrade_options = config
        .test_case_params
        .version_upgrade
        .unwrap_or(DEFAULT_UPGRADE_OPTIONS);

    let version_for_upgrade = upgrade_options.version;
    let session_for_upgrade = upgrade_options.session;

    connection.schedule_upgrade(version_for_upgrade, session_for_upgrade)?;

    let session_after_upgrade = session_for_upgrade + 1;

    let block_number = wait_for_at_least_session(&connection, session_after_upgrade)?;
    wait_for_finalized_block(&connection, block_number)?;

    Ok(())
}

// A test that schedules a version upgrade that supposed to fail, awaits it, and checks if finalization stopped.
// It's up to the user of this test to ensure that version upgrade will actually break finalization (non-compatible change in protocol).
pub fn schedule_doomed_version_change_and_veirfy_that_finalization_stopped(
    config: &Config,
) -> anyhow::Result<()> {
    let connection = config.create_root_connection();
    let upgrade_options = config
        .test_case_params
        .version_upgrade
        .unwrap_or(DEFAULT_UPGRADE_OPTIONS);

    let version_for_upgrade = upgrade_options.version;
    let session_for_upgrade = upgrade_options.session;

    // check if anything was finalized
    wait_for_finalized_block(&connection, 1)?;

    connection.schedule_upgrade(version_for_upgrade, session_for_upgrade)?;

    let session_after_upgrade = session_for_upgrade + upgrade_options.finalization_wait_sessions;
    let first_block_in_upgrade_session = get_session_first_block(&connection, session_for_upgrade);

    let block_number = wait_for_at_least_session(&connection, session_after_upgrade)?;
    let finalized_block = match connection.as_connection().get_finalized_head()? {
        Some(hash) => connection.as_connection().get_header(Some(hash))?,
        None => return Err("somehow no block was finalized (even we saw one)"),
    };

    let finalized_block = match finalized_block {
        Some(block) => block.number,
        None => return Err("unable "),
    };

    // check if finalization is still behind the upgrade-session
    assert!(finalized_block <= first_block_in_upgrade_session);

    Ok(())
}

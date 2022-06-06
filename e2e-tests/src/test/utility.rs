use std::error::Error;

use log::info;

use crate::{
    accounts::{get_sudo_key, NodeKeys},
    config::create_root_connection,
    transfer::setup_for_transfer,
};
use sp_core::Pair;
use substrate_api_client::{compose_call, compose_extrinsic, GenericAddress, XtStatus};

use aleph_client::{
    get_current_session, set_keys, wait_for_session, AnyConnection, KeyPair, RootConnection,
    SessionKeys, SignedConnection,
};
use codec::Compact;

use crate::config::Config;

pub fn batch_transactions(config: &Config) -> anyhow::Result<()> {
    const NUMBER_OF_TRANSACTIONS: usize = 100;

    let (connection, to) = setup_for_transfer(config);

    let call = compose_call!(
        connection.as_connection().metadata,
        "Balances",
        "transfer",
        GenericAddress::Id(to),
        Compact(1000u128)
    );
    let mut transactions = Vec::new();
    for _i in 0..NUMBER_OF_TRANSACTIONS {
        transactions.push(call.clone());
    }

    let extrinsic =
        compose_extrinsic!(connection.as_connection(), "Utility", "batch", transactions);

    let finalized_block_hash = connection
        .as_connection()
        .send_extrinsic(extrinsic.hex_encode(), XtStatus::Finalized)
        .expect("Could not send extrinsc")
        .expect("Could not get tx hash");
    info!(
        "[+] A batch of {} transactions was included in finalized {} block.",
        NUMBER_OF_TRANSACTIONS, finalized_block_hash
    );

    Ok(())
}

pub const ZERO_SESSION_KEYS: SessionKeys = SessionKeys {
    aura: [0; 32],
    aleph: [0; 32],
};

/// Changes keys of the first node described by the `validator_seeds` list to some `zero` values,
/// making it impossible to create new legal blocks.
pub fn disable_validator(config: &Config) -> anyhow::Result<()> {
    let root_connection = create_root_connection();

    let validators_controller = config.node_keys().controller_key;

    let controller_connection = SignedConnection::new(&config.node, validators_controller);
    set_keys(&controller_connection, ZERO_SESSION_KEYS, XtStatus::InBlock);

    // wait until our node is forced to use new keys, i.e. current session + 2
    let current_session = get_current_session(&root_connection);
    wait_for_session(&root_connection, current_session + 2)?;

    Ok(())
}

/// Rotates the keys of the first node described by the `validator_seeds` list,
/// making it able to rejoin the `consensus`.
pub fn enable_validator(config: &Config) -> anyhow::Result<()> {
    let root_connection = create_root_connection();

    let validators_controller = config.node_keys().controller_key;

    let validator_keys = rotate_keys(&root_connection).expect("Failed to retrieve keys from chain");
    let controller_connection = SignedConnection::new(node, validators_controller);
    set_keys(&controller_connection, validator_keys, XtStatus::InBlock);

    // wait until our node is forced to use new keys, i.e. current session + 2
    let current_session = get_current_session(&root_connection);
    wait_for_session(&root_connection, current_session + 2)?;

    Ok(())
}

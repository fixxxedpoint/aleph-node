use std::error::Error;

use log::info;

use crate::{
    accounts::{get_sudo_key, NodeKeypairs},
    transfer::setup_for_transfer,
};
use sp_core::Pair;
use substrate_api_client::{compose_call, compose_extrinsic, GenericAddress, XtStatus};

use aleph_client::{
    get_current_session, set_keys, wait_for_session, AnyConnection, RootConnection, SessionKeys,
    SignedConnection,
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

pub fn disable_validator(config: &Config, validator_keypairs: &NodeKeypairs) -> anyhow::Result<()> {
    // if config.validators_count < 2 {
    //     return Err(Error(""));
    // }

    // let root_connection: RootConnection =
    //     SignedConnection::new(config.node, get_sudo_key(config)).into();
    // let validator_keys = rotate_keys(&root_connection).expect("Failed to retrieve keys from chain");

    // let node_keypairs = NodeKeypairs::from(validator_seed);
    let controller_connection =
        SignedConnection::new(&config.node, validator_keypairs.controller_key.clone());
    let fixed_invalid_validator_keys = SessionKeys {
        aura: [0; 32],
        aleph: [0; 32],
    };
    set_keys(
        &controller_connection,
        fixed_invalid_validator_keys,
        XtStatus::InBlock,
    );

    let root_connection =
        RootConnection::from(SignedConnection::new(&config.node, get_sudo_key(config)));
    // wait until our node is forced to use new keys
    let current_session = get_current_session(&root_connection);
    wait_for_session(&root_connection, current_session + 2)?;

    Ok(())
}

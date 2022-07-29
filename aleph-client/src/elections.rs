use pallet_elections::{CommitteeSeats, EraValidators};
use primitives::SessionIndex;
use sp_core::H256;
use substrate_api_client::AccountId;

use crate::{get_block_hash, get_session_period, AnyConnection};

// TODO this is wrong - it should be returning CommitteeSeats, not u32
pub fn get_committee_size<C: AnyConnection>(
    connection: &C,
    block_hash: Option<H256>,
) -> CommitteeSeats {
    connection
        .as_connection()
        .get_storage_value("Elections", "CommitteeSize", block_hash)
        .expect("Failed to decode CommitteeSize extrinsic!")
        .unwrap_or_else(|| {
            panic!(
                "Failed to obtain CommitteeSize for block hash: {:?}.",
                block_hash
            )
        })
}

pub fn get_committee_seats<C: AnyConnection>(
    connection: &C,
    block_hash: Option<H256>,
) -> CommitteeSeats {
    connection
        .as_connection()
        .get_storage_value("Elections", "CommitteeSize", block_hash)
        .expect("Failed to decode CommitteeSize extrinsic!")
        .unwrap_or_else(|| {
            panic!(
                "Failed to obtain CommitteeSize for block hash: {:?}.",
                block_hash
            )
        })
}

pub fn get_validator_block_count<C: AnyConnection>(
    connection: &C,
    account_id: &AccountId,
    block_hash: Option<H256>,
) -> Option<u32> {
    connection
        .as_connection()
        .get_storage_map(
            "Elections",
            "SessionValidatorBlockCount",
            account_id,
            block_hash,
        )
        .expect("Failed to decode SessionValidatorBlockCount extrinsic!")
}

pub fn get_era_validators<C: AnyConnection>(
    connection: &C,
    session_index: SessionIndex,
) -> EraValidators<AccountId> {
    let session_period = get_session_period(connection);
    let block_number = session_period * session_index;
    // let block_number = 10;
    let block_hash = get_block_hash(connection, block_number);
    print!(
        "about to retrieve CurrentEraValidators from block {} - {}",
        block_number, block_hash
    );
    connection.read_storage_value_from_block("Elections", "CurrentEraValidators", Some(block_hash))
}
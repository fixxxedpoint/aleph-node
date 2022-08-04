use primitives::SessionIndex;
pub use primitives::{CommitteeSeats, EraValidators};
use sp_core::H256;
use substrate_api_client::AccountId;

use crate::{get_block_hash, get_session_period, AnyConnection};

const PALLET: &str = "Elections";

pub fn get_committee_seats<C: AnyConnection>(
    connection: &C,
    block_hash: Option<H256>,
) -> CommitteeSeats {
    connection
        .as_connection()
        .get_storage_value(PALLET, "CommitteeSize", block_hash)
        .expect("Failed to obtain CommitteeSize extrinsic!")
        .unwrap_or_else(|| {
            panic!(
                "Failed to decode CommitteeSize for block hash: {:?}.",
                block_hash
            )
        })
}

pub fn get_next_era_committee_seats<C: AnyConnection>(connection: &C) -> CommitteeSeats {
    connection.read_storage_value(PALLET, "NextEraCommitteeSize")
}

pub fn get_validator_block_count<C: AnyConnection>(
    connection: &C,
    account_id: &AccountId,
    block_hash: Option<H256>,
) -> Option<u32> {
    connection
        .as_connection()
        .get_storage_map(PALLET, "SessionValidatorBlockCount", account_id, block_hash)
        .expect("Failed to obtain SessionValidatorBlockCount extrinsic!")
}

pub fn get_current_era_validators<C: AnyConnection>(connection: &C) -> EraValidators<AccountId> {
    connection.read_storage_value(PALLET, "CurrentEraValidators")
}

pub fn get_current_era_reserved_validators<C: AnyConnection>(connection: &C) -> Vec<AccountId> {
    get_current_era_validators(connection).reserved
}

pub fn get_current_era_non_reserved_validators<C: AnyConnection>(connection: &C) -> Vec<AccountId> {
    get_current_era_validators(connection).non_reserved
}

pub fn get_next_era_reserved_validators<C: AnyConnection>(connection: &C) -> Vec<AccountId> {
    connection.read_storage_value(PALLET, "NextEraReservedValidators")
}

pub fn get_next_era_non_reserved_validators<C: AnyConnection>(connection: &C) -> Vec<AccountId> {
    connection.read_storage_value(PALLET, "NextEraNonReservedValidators")
}

pub fn get_era_validators<C: AnyConnection>(
    connection: &C,
    session_index: SessionIndex,
) -> EraValidators<AccountId> {
    let session_period = get_session_period(connection);
    let block_number = session_period * session_index;
    let block_hash = get_block_hash(connection, block_number);
    connection
        .read_storage_value_from_block("Elections", "CurrentEraValidators", Some(block_hash))
        .expect("Elections/CurrentEraValidators should be set to some value")
}

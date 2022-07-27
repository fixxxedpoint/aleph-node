use pallet_elections::EraValidators;
use sp_core::H256;
use substrate_api_client::AccountId;

use crate::{AnyConnection, AnyConnectionExtra};

pub fn get_committee_size<C: AnyConnection>(connection: &C, block_hash: Option<H256>) -> u32 {
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

pub fn get_validators<C: AnyConnectionExtra>(
    connection: &C,
    block_hash: H256,
) -> EraValidators<AccountId> {
    connection
        .read_storage_value("Elections", "CurrentEraValidators", block_hash)
        .expect("Failed to decode SessionValidatorBlockCount extrinsic!")
}

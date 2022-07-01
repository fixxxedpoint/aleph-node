use std::collections::BTreeMap;

use codec::{Compact, Decode, Encode};
use frame_support::BoundedVec;
use pallet_staking::{
    Exposure, MaxUnlockingChunks, RewardDestination, UnlockChunk, ValidatorPrefs,
};
use primitives::EraIndex;
use rayon::prelude::*;
use sp_core::{Pair, H256};
use sp_runtime::Perbill;
use substrate_api_client::{
    compose_call, compose_extrinsic, AccountId, Balance, ExtrinsicParams, GenericAddress, XtStatus,
};

use crate::{
    account_from_keypair, create_connection, locks, send_xt, wait_for_session, AnyConnection,
    BlockNumber, KeyPair, RootConnection, SignedConnection,
};

pub fn bond(
    connection: &SignedConnection,
    initial_stake: Balance,
    controller_account_id: &AccountId,
    status: XtStatus,
) {
    let controller_account_id = GenericAddress::Id(controller_account_id.clone());

    let xt = connection.as_connection().staking_bond(
        controller_account_id,
        initial_stake,
        RewardDestination::Staked,
    );
    send_xt(connection, xt, Some("bond"), status);
}

pub fn multi_bond(node: &str, bonders: &[KeyPair], stake: Balance) {
    bonders.par_iter().for_each(|bonder| {
        let connection = create_connection(node)
            .set_signer(bonder.clone())
            .try_into()
            .expect("Signer has been set");

        let controller_account = account_from_keypair(bonder);
        bond(&connection, stake, &controller_account, XtStatus::InBlock);
    });
}

pub fn validate(
    connection: &SignedConnection,
    validator_commission_percentage: u8,
    status: XtStatus,
) {
    let prefs = ValidatorPrefs {
        blocked: false,
        commission: Perbill::from_percent(validator_commission_percentage as u32),
    };
    let xt = compose_extrinsic!(connection.as_connection(), "Staking", "validate", prefs);
    send_xt(connection, xt, Some("validate"), status);
}

pub fn set_staking_limits(
    connection: &RootConnection,
    minimal_nominator_stake: u128,
    minimal_validator_stake: u128,
    max_nominators_count: Option<u32>,
    max_validators_count: Option<u32>,
    status: XtStatus,
) {
    let set_staking_limits_call = compose_call!(
        connection.as_connection().metadata,
        "Staking",
        "set_staking_limits",
        minimal_nominator_stake,
        minimal_validator_stake,
        max_nominators_count,
        max_validators_count,
        0_u8
    );
    let xt = compose_extrinsic!(
        connection.as_connection(),
        "Sudo",
        "sudo",
        set_staking_limits_call
    );
    send_xt(connection, xt, Some("set_staking_limits"), status);
}

pub fn force_new_era(connection: &RootConnection, status: XtStatus) {
    let force_new_era_call = compose_call!(
        connection.as_connection().metadata,
        "Staking",
        "force_new_era"
    );
    let xt = compose_extrinsic!(
        connection.as_connection(),
        "Sudo",
        "sudo",
        force_new_era_call
    );
    send_xt(connection, xt, Some("force_new_era"), status);
}

pub fn wait_for_full_era_completion<C: AnyConnection>(connection: &C) -> anyhow::Result<EraIndex> {
    // staking works in such a way, that when we request a controller to be a validator in era N,
    // then the changes are applied in the era N+1 (so the new validator is receiving points in N+1),
    // so that we need N+1 to finish in order to claim the reward in era N+2 for the N+1 era
    wait_for_era_completion(connection, get_current_era(connection) + 2)
}

pub fn wait_for_next_era<C: AnyConnection>(connection: &C) -> anyhow::Result<EraIndex> {
    wait_for_era_completion(connection, get_current_era(connection) + 1)
}

fn wait_for_era_completion<C: AnyConnection>(
    connection: &C,
    next_era_index: EraIndex,
) -> anyhow::Result<EraIndex> {
    let sessions_per_era: u32 = connection
        .as_connection()
        .get_constant("Staking", "SessionsPerEra")
        .expect("Failed to decode SessionsPerEra extrinsic!");
    let first_session_in_next_era = next_era_index * sessions_per_era;
    wait_for_session(connection, first_session_in_next_era)?;
    Ok(next_era_index)
}

pub fn get_sessions_per_era<C: AnyConnection>(connection: &C) -> u32 {
    connection
        .as_connection()
        .get_constant("Staking", "SessionsPerEra")
        .expect("Failed to decode SessionsPerEra extrinsic!")
}

pub fn get_era<C: AnyConnection>(connection: &C, block: Option<H256>) -> EraIndex {
    connection
        .as_connection()
        .get_storage_value("Staking", "ActiveEra", block)
        .expect("Failed to decode ActiveEra extrinsic!")
        .expect("ActiveEra is empty in the storage!")
}

pub fn get_current_era<C: AnyConnection>(connection: &C) -> EraIndex {
    get_era(connection, None)
}

pub fn payout_stakers(
    stash_connection: &SignedConnection,
    stash_account: &AccountId,
    era_number: BlockNumber,
) {
    let xt = compose_extrinsic!(
        stash_connection.as_connection(),
        "Staking",
        "payout_stakers",
        stash_account,
        era_number
    );

    send_xt(
        stash_connection,
        xt,
        Some("payout stakers"),
        XtStatus::InBlock,
    );
}

pub fn payout_stakers_and_assert_locked_balance(
    stash_connection: &SignedConnection,
    accounts_to_check_balance: &[AccountId],
    stash_account: &AccountId,
    era: BlockNumber,
) {
    let locked_stash_balances_before_payout = locks(stash_connection, accounts_to_check_balance);
    payout_stakers(stash_connection, stash_account, era - 1);
    let locked_stash_balances_after_payout = locks(stash_connection, accounts_to_check_balance);
    locked_stash_balances_before_payout.iter()
        .zip(locked_stash_balances_after_payout.iter())
        .zip(accounts_to_check_balance.iter())
        .for_each(|((balances_before, balances_after), account_id)| {
            assert!(balances_after[0].amount > balances_before[0].amount,
                    "Expected payout to be positive in locked balance for account {}. Balance before: {}, balance after: {}",
                    account_id, balances_before[0].amount, balances_after[0].amount);
        });
}

pub fn batch_bond(
    connection: &RootConnection,
    stash_controller_accounts: &[(&AccountId, &AccountId)],
    bond_value: u128,
    reward_destination: RewardDestination<GenericAddress>,
) {
    let metadata = &connection.as_connection().metadata;

    let batch_bond_calls = stash_controller_accounts
        .iter()
        .cloned()
        .map(|(stash_account, controller_account)| {
            let bond_call = compose_call!(
                metadata,
                "Staking",
                "bond",
                GenericAddress::Id(controller_account.clone()),
                Compact(bond_value),
                reward_destination.clone()
            );
            compose_call!(
                metadata,
                "Sudo",
                "sudo_as",
                GenericAddress::Id(stash_account.clone()),
                bond_call
            )
        })
        .collect::<Vec<_>>();

    let xt = compose_extrinsic!(
        connection.as_connection(),
        "Utility",
        "batch",
        batch_bond_calls
    );
    send_xt(
        connection,
        xt,
        Some("batch of bond calls"),
        XtStatus::InBlock,
    );
}

pub fn nominate(connection: &SignedConnection, nominee_account_id: &AccountId) {
    let xt = connection
        .as_connection()
        .staking_nominate(vec![GenericAddress::Id(nominee_account_id.clone())]);
    send_xt(connection, xt, Some("nominate"), XtStatus::InBlock);
}

pub fn batch_nominate(
    connection: &RootConnection,
    nominator_nominee_pairs: &[(&AccountId, &AccountId)],
) {
    let metadata = &connection.as_connection().metadata;

    let batch_nominate_calls = nominator_nominee_pairs
        .iter()
        .cloned()
        .map(|(nominator, nominee)| {
            let nominate_call = compose_call!(
                metadata,
                "Staking",
                "nominate",
                vec![GenericAddress::Id(nominee.clone())]
            );
            compose_call!(
                metadata,
                "Sudo",
                "sudo_as",
                GenericAddress::Id(nominator.clone()),
                nominate_call
            )
        })
        .collect::<Vec<_>>();

    let xt = compose_extrinsic!(
        connection.as_connection(),
        "Utility",
        "batch",
        batch_nominate_calls
    );
    send_xt(
        connection,
        xt,
        Some("batch of nominate calls"),
        XtStatus::InBlock,
    );
}

pub fn bonded<C: AnyConnection>(connection: &C, stash: &KeyPair) -> Option<AccountId> {
    let account_id = AccountId::from(stash.public());
    connection
        .as_connection()
        .get_storage_map("Staking", "Bonded", &account_id, None)
        .unwrap_or_else(|_| panic!("Failed to obtain Bonded for account id {}", account_id))
}

/// Since PR #10982 changed `pallet_staking::StakingLedger` to be generic over
/// `T: pallet_staking::Config` (somehow breaking consistency with similar structures in other
/// pallets) we have no easy way of retrieving ledgers from storage. Thus, we chose cloning
/// (relevant part of) this struct instead of implementing `Config` trait.
#[derive(PartialEq, Eq, Clone, Debug, Encode, Decode)]
pub struct StakingLedger {
    pub stash: AccountId,
    #[codec(compact)]
    pub total: Balance,
    #[codec(compact)]
    pub active: Balance,
    pub unlocking: BoundedVec<UnlockChunk<Balance>, MaxUnlockingChunks>,
}

pub fn ledger<C: AnyConnection>(connection: &C, controller: &KeyPair) -> Option<StakingLedger> {
    let account_id = AccountId::from(controller.public());
    connection
        .as_connection()
        .get_storage_map("Staking", "Ledger", &account_id, None)
        .unwrap_or_else(|_| panic!("Failed to obtain Ledger for account id {}", account_id))
}

pub fn get_payout_for_era<C: AnyConnection>(connection: &C, era: EraIndex) -> u128 {
    connection
        .as_connection()
        .get_storage_map("Staking", "ErasValidatorReward", era, None)
        .expect("Failed to decode ErasValidatorReward")
        .expect("ErasValidatoReward is empty in the storage")
}

pub fn get_exposure<C: AnyConnection>(
    connection: &C,
    era: EraIndex,
    account_id: &AccountId,
    block_hash: Option<H256>,
) -> Exposure<AccountId, u128> {
    connection
        .as_connection()
        .get_storage_double_map("Staking", "ErasStakers", era, account_id, block_hash)
        .expect("Failed to decode ErasStakers extrinsic!")
        .unwrap_or_else(|| panic!("Failed to obtain ErasStakers for era {}.", era))
}

pub type RewardPoint = u32;

/// Helper to decode reward points for an era without the need to fill in a generic parameter.
/// Reward points of an era. Used to split era total payout between validators.
///
/// This points will be used to reward validators and their respective nominators.
#[derive(Clone, Decode, Default)]
pub struct EraRewardPoints {
    /// Total number of points. Equals the sum of reward points for each validator.
    pub total: RewardPoint,
    /// The reward points earned by a given validator.
    pub individual: BTreeMap<AccountId, RewardPoint>,
}

pub fn get_era_reward_points<C: AnyConnection>(
    connection: &C,
    era: EraIndex,
    block_hash: Option<H256>,
) -> Option<EraRewardPoints> {
    connection
        .as_connection()
        .get_storage_map("Staking", "ErasRewardPoints", era, block_hash)
        .unwrap_or_else(|e| {
            panic!(
                "Failed to obtain ErasRewardPoints for era {} at block {:?}: {}",
                era, block_hash, e
            )
        })
}

use aleph_client::{
    account_from_keypair, balances_batch_transfer, balances_transfer, get_current_era,
    get_current_session, get_sessions_per_era, send_xt, staking_force_new_era,
    wait_for_full_era_completion, wait_for_next_era, wait_for_session, AnyConnection,
    EraValidators, SignedConnection,
};
use log::info;
use primitives::{staking::MIN_VALIDATOR_BOND, Balance, EraIndex, SessionIndex, TOKEN};
use substrate_api_client::{AccountId, XtStatus};

use crate::{
    accounts::{get_validators_keys, get_validators_seeds, NodeKeys},
    rewards::{
        check_points, get_era_from_session, get_member_accounts, get_members_for_session,
        reset_validator_keys, set_invalid_keys_for_validator, setup_validators,
    },
    Config,
};

// Maximum difference between fractions of total reward that a validator gets.
// Two values are compared: one calculated in tests and the other one based on data
// retrieved from pallet Staking.
const MAX_DIFFERENCE: f64 = 0.07;

fn validators_bond_extra_stakes(config: &Config, additional_stakes: Vec<Balance>) {
    let node = &config.node;
    let root_connection = config.create_root_connection();

    let accounts_keys: Vec<NodeKeys> = get_validators_seeds(config)
        .into_iter()
        .map(|seed| seed.into())
        .collect();

    let controller_accounts: Vec<AccountId> = accounts_keys
        .iter()
        .map(|account_keys| account_from_keypair(&account_keys.controller))
        .collect();

    // funds to cover fees
    balances_batch_transfer(&root_connection.as_signed(), controller_accounts, TOKEN);

    accounts_keys.iter().zip(additional_stakes.iter()).for_each(
        |(account_keys, additional_stake)| {
            let validator_id = account_from_keypair(&account_keys.validator);

            // Additional TOKEN to cover fees
            balances_transfer(
                &root_connection.as_signed(),
                &validator_id,
                *additional_stake + TOKEN,
                XtStatus::Finalized,
            );
            let stash_connection = SignedConnection::new(node, account_keys.validator.clone());
            let xt = stash_connection
                .as_connection()
                .staking_bond_extra(*additional_stake);
            send_xt(
                &stash_connection,
                xt,
                Some("bond_extra"),
                XtStatus::Finalized,
            );
        },
    );
}

fn check_points_after_force_new_era(
    connection: &SignedConnection,
    start_session: SessionIndex,
    start_era: EraIndex,
    reserved_members: &Vec<AccountId>,
    non_reserved_members: &Vec<AccountId>,
    members_per_session: u32,
    max_relative_difference: f64,
) -> anyhow::Result<()> {
    let era_validators = EraValidators {
        reserved: reserved_members.clone(),
        non_reserved: non_reserved_members.clone(),
    };
    // Once a new era is forced in session k, the new era does not come into effect until session
    // k + 2; we test points:
    // 1) immediately following the call in session k,
    // 2) in the interim session k + 1,
    // 3) in session k + 2, the first session of the new era.
    for idx in 0..3 {
        let session_to_check = start_session + idx;
        let era_to_check = start_era + idx / 2;

        info!(
            "Testing points | era: {}, session: {}",
            era_to_check, session_to_check
        );

        let (members_active, members_bench) = get_members_for_session(
            connection,
            members_per_session,
            &era_validators,
            session_to_check,
        );

        check_points(
            connection,
            session_to_check,
            era_to_check,
            members_active,
            members_bench,
            members_per_session,
            max_relative_difference,
        )?;
    }
    Ok(())
}

pub fn points_basic(config: &Config) -> anyhow::Result<()> {
    let (era_validators, committee_size, era) = setup_validators(config)?;

    let node = &config.node;
    let accounts = get_validators_keys(config);
    let sender = accounts.first().expect("Using default accounts").to_owned();
    let connection = SignedConnection::new(node, sender);

    let sessions_per_era = get_sessions_per_era(&connection);
    let start_new_era_session = era * sessions_per_era;
    let end_new_era_session = sessions_per_era * wait_for_next_era(&connection)?;

    info!(
        "Checking rewards for sessions {}..{}.",
        start_new_era_session, end_new_era_session
    );

    for session in start_new_era_session..end_new_era_session {
        let (members_active, members_bench) =
            get_members_for_session(&connection, committee_size, &era_validators, session);

        let era = get_era_from_session(&connection, session);

        check_points(
            &connection,
            session,
            era,
            members_active,
            members_bench,
            committee_size,
            MAX_DIFFERENCE,
        )?
    }

    Ok(())
}

/// Runs a chain, bonds extra stakes to validator accounts and checks that reward points
/// are calculated correctly afterward.
pub fn points_stake_change(config: &Config) -> anyhow::Result<()> {
    let (era_validators, committee_size, era) = setup_validators(config)?;

    let node = &config.node;
    let accounts = get_validators_keys(config);
    let sender = accounts.first().expect("Using default accounts").to_owned();
    let connection = SignedConnection::new(node, sender);

    validators_bond_extra_stakes(
        config,
        [
            8 * MIN_VALIDATOR_BOND,
            6 * MIN_VALIDATOR_BOND,
            4 * MIN_VALIDATOR_BOND,
            2 * MIN_VALIDATOR_BOND,
            0,
        ]
        .to_vec(),
    );

    let sessions_per_era = get_sessions_per_era(&connection);
    let start_era_session = era * sessions_per_era;
    let end_era_session = wait_for_next_era(&connection)? * sessions_per_era;

    info!(
        "Checking rewards for sessions {}..{}.",
        start_era_session, end_era_session
    );

    for session in start_era_session..end_era_session {
        let (members_active, members_bench) =
            get_members_for_session(&connection, committee_size, &era_validators, session);

        check_points(
            &connection,
            session,
            era,
            members_active,
            members_bench,
            committee_size,
            MAX_DIFFERENCE,
        )?
    }

    Ok(())
}

/// Runs a chain, sets invalid session keys for one validator, re-sets the keys to valid ones
/// and checks that reward points are calculated correctly afterward.
pub fn disable_node(config: &Config) -> anyhow::Result<()> {
    const MAX_DIFFERENCE: f64 = 0.07;

    let (era_validators, committee_size, era) = setup_validators(config)?;

    let root_connection = config.create_root_connection();
    let sessions_per_era = get_sessions_per_era(&root_connection);
    let (reserved_members, non_reserved_members) = get_member_accounts(&root_connection, 0);

    let start_session = era * sessions_per_era;

    let controller_connection = SignedConnection::new(&config.node, config.node_keys().controller);
    // this should `disable` this node by setting invalid session_keys
    set_invalid_keys_for_validator(&controller_connection)?;
    // this should `re-enable` this node, i.e. by means of the `rotate keys` procedure
    reset_validator_keys(&controller_connection)?;

    let era = wait_for_full_era_completion(&root_connection)?;

    let end_session = era * sessions_per_era;

    info!(
        "Checking rewards for sessions {}..{}.",
        start_session, end_session
    );

    for session in start_session..end_session {
        let (members_active, members_bench) = get_members_for_session(
            &controller_connection,
            committee_size,
            &era_validators,
            session,
        );

        let era = get_era_from_session(&controller_connection, session);

        check_points(
            &controller_connection,
            session,
            era,
            members_active,
            members_bench,
            committee_size,
            MAX_DIFFERENCE,
        )?;
    }

    Ok(())
}

/// Runs a chain, forces a new era to begin, checks that reward points are calculated correctly
/// for 3 sessions: 1) immediately following the forcing call, 2) in the subsequent, interim
/// session, when the new era has not yet started, 3) in the next session, second one after
/// the call, when the new era has already begun.
pub fn force_new_era(config: &Config) -> anyhow::Result<()> {
    const MAX_DIFFERENCE: f64 = 0.07;

    let (era_validators, committee_size, start_era) = setup_validators(config)?;

    let node = &config.node;
    let accounts = get_validators_keys(config);
    let sender = accounts.first().expect("Using default accounts").to_owned();
    let connection = SignedConnection::new(node, sender);
    let root_connection = config.create_root_connection();

    let start_session = get_current_session(&connection);
    info!("Start | era: {}, session: {}", start_era, start_session);

    staking_force_new_era(&root_connection, XtStatus::Finalized);

    wait_for_session(&connection, start_session + 2)?;
    let current_era = get_current_era(&connection);
    let current_session = get_current_session(&connection);
    info!(
        "After ForceNewEra | era: {}, session: {}",
        current_era, current_session
    );

    check_points_after_force_new_era(
        &connection,
        start_session,
        start_era,
        &era_validators.reserved,
        &era_validators.non_reserved,
        committee_size,
        MAX_DIFFERENCE,
    )?;
    Ok(())
}

/// Change stake and force new era: checks if reward points are calculated properly
/// in a scenario in which stakes are changed for each validator, and then a new era is forced.
///
/// Expected behaviour: until the next (forced) era, rewards are calculated using old stakes,
/// and after two sessions (required for a new era to be forced) they are adjusted to the new
/// stakes.
pub fn change_stake_and_force_new_era(config: &Config) -> anyhow::Result<()> {
    let (era_validators, committee_size, start_era) = setup_validators(config)?;

    let node = &config.node;
    let accounts = get_validators_keys(config);
    let sender = accounts.first().expect("Using default accounts").to_owned();
    let connection = SignedConnection::new(node, sender);
    let root_connection = config.create_root_connection();

    let start_session = get_current_session(&connection);
    info!("Start | era: {}, session: {}", start_era, start_session);

    validators_bond_extra_stakes(
        config,
        [
            7 * MIN_VALIDATOR_BOND,
            2 * MIN_VALIDATOR_BOND,
            11 * MIN_VALIDATOR_BOND,
            0,
            4 * MIN_VALIDATOR_BOND,
        ]
        .to_vec(),
    );

    staking_force_new_era(&root_connection, XtStatus::Finalized);

    wait_for_session(&connection, start_session + 2)?;
    let current_era = get_current_era(&connection);
    let current_session = get_current_session(&connection);
    info!(
        "After ForceNewEra | era: {}, session: {}",
        current_era, current_session
    );

    check_points_after_force_new_era(
        &connection,
        start_session,
        start_era,
        &era_validators.reserved,
        &era_validators.non_reserved,
        committee_size,
        MAX_DIFFERENCE,
    )?;
    Ok(())
}

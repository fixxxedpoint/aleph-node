use aleph_client::{
    account_from_keypair, change_validators, get_current_era, get_current_session,
    get_era_validators, get_sessions_per_era, staking_force_new_era, wait_for_full_era_completion,
    wait_for_next_era, wait_for_session, KeyPair, SignedConnection,
};
use log::info;
use primitives::{staking::MIN_VALIDATOR_BOND, SessionIndex};
use substrate_api_client::{AccountId, XtStatus};

use crate::{
    accounts::get_validators_keys,
    rewards::{
        check_points, get_bench_members, reset_validator_keys, set_invalid_keys_for_validator,
        validators_bond_extra_stakes,
    },
    Config,
};

fn get_reserved_members<C: AnyConnection>(
    connection: &C,
    session_index: SessionIndex,
) -> Vec<AccountId> {
    get_era_validators(connection, session_index).reserved
}

fn get_non_reserved_members<C: AnyConnection>(
    connection: &C,
    session_index: SessionIndex,
) -> Vec<AccountId> {
    get_era_validators(connection, session_index).non_reserved
}

fn get_non_reserved_members_for_session(config: &Config, session: SessionIndex) -> Vec<AccountId> {
    let root_connection = config.create_root_connection();
    let session_validators = get_session_validators(root_connection, session);
    let (_, non_reserved) = get_member_accounts(&root_connection, session);
    let free_seats = members.non_reserved + members.reserved - session_validators.len();

    let mut non_reserved = vec![];

    let non_reserved_nodes_order_from_runtime = members.non_reserved;
    let non_reserved_nodes_order_from_runtime_len = non_reserved_nodes_order_from_runtime.len();

    for i in (free_seats * session)..(free_seats * (session + 1)) {
        non_reserved.push(
            non_reserved_nodes_order_from_runtime
                [i as usize % non_reserved_nodes_order_from_runtime_len]
                .clone(),
        );
    }

    non_reserved.iter().map(account_from_keypair).collect()
}

fn get_member_accounts<C: AnyConnection>(
    connection: &C,
    session_index: SessionIndex,
) -> (Vec<AccountId>, Vec<AccountId>) {
    (
        get_reserved_members(connection, session_index)
            .iter()
            .map(account_from_keypair)
            .collect(),
        get_non_reserved_members(connection, session_index)
            .iter()
            .map(account_from_keypair)
            .collect(),
    )
}

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
    config: &Config,
    connection: &SignedConnection,
    start_session: SessionIndex,
    start_era: EraIndex,
    reserved_members: Vec<AccountId>,
    non_reserved_members: Vec<AccountId>,
    max_relative_difference: f64,
) -> anyhow::Result<()> {
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

        let non_reserved_members_for_session =
            get_non_reserved_members_for_session(config, session_to_check);
        let members_bench = get_bench_members(
            non_reserved_members.clone(),
            &non_reserved_members_for_session,
        );
        let members_active = reserved_members
            .clone()
            .into_iter()
            .chain(non_reserved_members_for_session);

        check_points(
            connection,
            session_to_check,
            era_to_check,
            members_active,
            members_bench,
            max_relative_difference,
        )?;
    }
    Ok(())
}

pub fn points_basic(config: &Config) -> anyhow::Result<()> {
    const MAX_DIFFERENCE: f64 = 0.07;

    let node = &config.node;
    let accounts = get_validators_keys(config);
    let sender = accounts.first().expect("Using default accounts").to_owned();
    let connection = SignedConnection::new(node, sender);
    let root_connection = config.create_root_connection();
    let current_session = get_current_session(&root_connection);

    let (reserved_members, non_reserved_members) =
        get_member_accounts(&root_connection, current_session);

    // why not?
    let members_count = reserved_members.len() + non_reserved_members.len();
    let byzantine_threshold = (members_count - 1) / 3;

    change_validators(
        &root_connection,
        Some(reserved_members.clone()),
        Some(non_reserved_members.clone()),
        Some(members_count - byzantine_threshold),
        XtStatus::Finalized,
    );

    let sessions_per_era = get_sessions_per_era(&connection);
    let era = wait_for_next_era(&connection)?;
    let start_new_era_session = era * sessions_per_era;
    let end_new_era_session = sessions_per_era * wait_for_next_era(&connection)?;

    info!(
        "Checking rewards for sessions {}..{}.",
        start_new_era_session, end_new_era_session
    );

    for session in start_new_era_session..end_new_era_session {
        let non_reserved_for_session = get_non_reserved_members_for_session(config, session);
        let members_bench =
            get_bench_members(non_reserved_members.clone(), &non_reserved_for_session);
        let members_active = reserved_members
            .clone()
            .into_iter()
            .chain(non_reserved_for_session)
            .collect::<Vec<_>>();

        check_points(
            &connection,
            session,
            era,
            members_active,
            members_bench,
            MAX_DIFFERENCE,
        )?
    }

    Ok(())
}

/// Runs a chain, bonds extra stakes to validator accounts and checks that reward points
/// are calculated correctly afterward.
pub fn points_stake_change(config: &Config) -> anyhow::Result<()> {
    const MAX_DIFFERENCE: f64 = 0.07;
    const VALIDATORS_PER_SESSION: u32 = 4;

    let node = &config.node;
    let accounts = get_validators_keys(config);
    let sender = accounts.first().expect("Using default accounts").to_owned();
    let connection = SignedConnection::new(node, sender);
    let root_connection = config.create_root_connection();
    let current_session = get_current_session(&root_connection);

    let (reserved_members, non_reserved_members) =
        get_member_accounts(&root_connection, current_session);

    change_validators(
        &root_connection,
        Some(reserved_members.clone()),
        Some(non_reserved_members.clone()),
        Some(VALIDATORS_PER_SESSION),
        XtStatus::Finalized,
    );

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
    let era = wait_for_next_era(&connection)?;
    let start_era_session = era * sessions_per_era;
    let end_era_session = sessions_per_era * wait_for_next_era(&connection)?;

    info!(
        "Checking rewards for sessions {}..{}.",
        start_era_session, end_era_session
    );

    for session in start_era_session..end_era_session {
        let non_reserved_members_for_session =
            get_non_reserved_members_for_session(config, session);
        let members_bench = get_bench_members(
            non_reserved_members.clone(),
            &non_reserved_members_for_session,
        );
        let members_active = reserved_members
            .clone()
            .into_iter()
            .chain(non_reserved_members_for_session)
            .collect::<Vec<_>>();

        check_points(
            &connection,
            session,
            era,
            members_active,
            members_bench,
            MAX_DIFFERENCE,
        )?
    }

    Ok(())
}

/// Runs a chain, sets invalid session keys for one validator, re-sets the keys to valid ones
/// and checks that reward points are calculated correctly afterward.
pub fn disable_node(config: &Config) -> anyhow::Result<()> {
    const MAX_DIFFERENCE: f64 = 0.07;

    let root_connection = config.create_root_connection();
    let sessions_per_era = get_sessions_per_era(&root_connection);
    let (reserved_members, non_reserved_members) = get_member_accounts(&root_connection, 0);

    change_validators(
        &root_connection,
        Some(reserved_members.clone()),
        Some(non_reserved_members.clone()),
        Some(CommitteeSeats {
            reserved_seats: 2,
            non_reserved_seats: 2,
        }),
        XtStatus::Finalized,
    );

    let era = wait_for_next_era(&root_connection)?;
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
        let non_reserved_members_for_session =
            get_non_reserved_members_for_session(config, session);
        let members_bench = get_bench_members(
            non_reserved_members.clone(),
            &non_reserved_members_for_session,
        );
        let members_active = reserved_members
            .iter()
            .chain(non_reserved_members_for_session.iter())
            .cloned();

        let era = session / sessions_per_era;
        check_points(
            &controller_connection,
            session,
            era,
            members_active,
            members_bench,
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

    let node = &config.node;
    let accounts = get_validators_keys(config);
    let sender = accounts.first().expect("Using default accounts").to_owned();
    let connection = SignedConnection::new(node, sender);
    let root_connection = config.create_root_connection();

    let reserved_members: Vec<_> = get_reserved_members(&root_connection, 0);
    let non_reserved_members: Vec<_> = get_non_reserved_members(&root_connection, 0);

    change_validators(
        &root_connection,
        Some(reserved_members.clone()),
        Some(non_reserved_members.clone()),
        Some(CommitteeSeats {
            reserved_seats: 2,
            non_reserved_seats: 2,
        }),
        XtStatus::Finalized,
    );

    wait_for_full_era_completion(&connection)?;

    let start_era = get_current_era(&connection);
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
        config,
        &connection,
        start_session,
        start_era,
        reserved_members,
        non_reserved_members,
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
    let node = &config.node;
    let accounts = get_validators_keys(config);
    let sender = accounts.first().expect("Using default accounts").to_owned();
    let connection = SignedConnection::new(node, sender);

    let sudo = get_sudo_key(config);
    let root_connection = RootConnection::new(node, sudo);

    let (reserved_members, non_reserved_members) = get_member_accounts(config);

    change_validators(
        &root_connection,
        Some(reserved_members.clone()),
        Some(non_reserved_members.clone()),
        Some(CommitteeSeats {
            reserved_seats: 2,
            non_reserved_seats: 2,
        }),
        XtStatus::Finalized,
    );

    wait_for_full_era_completion(&connection)?;

    let start_era = get_current_era(&connection);
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
        config,
        &connection,
        start_session,
        start_era,
        reserved_members,
        non_reserved_members,
        MAX_DIFFERENCE,
    )?;
    Ok(())
}

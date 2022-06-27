use crate::{
    accounts::{accounts_seeds_to_keys, get_sudo_key, get_validators_keys, get_validators_seeds},
    Config,
};
use aleph_client::{
    change_validators, get_current_session, get_era, get_era_reward_points,
    get_era_reward_points_result, get_session, get_sessions_per_era, wait_for_finalized_block,
    wait_for_full_era_completion, wait_for_next_era, AnyConnection, EraRewardPoints, KeyPair,
    RewardPoint, RootConnection, SignedConnection,
};
use log::info;
use pallet_staking::Exposure;
use primitives::LENIENT_THRESHOLD;
use sp_core::{Pair, H256};
use sp_runtime::Perquintill;
use std::collections::BTreeMap;
use substrate_api_client::{AccountId, XtStatus};

use super::utility::disable_validator;

fn get_reserved_members(config: &Config) -> Vec<KeyPair> {
    get_validators_keys(config)[0..2].to_vec()
}

fn get_non_reserved_members(config: &Config) -> Vec<KeyPair> {
    get_validators_keys(config)[2..].to_vec()
}

fn get_non_reserved_members_for_session(config: &Config, session: u32) -> Vec<AccountId> {
    // Test assumption
    const FREE_SEATS: u32 = 2;

    let mut non_reserved = vec![];

    let validators_seeds = get_validators_seeds(config);
    let non_reserved_nodes_order_from_runtime = validators_seeds[2..].to_vec();
    let non_reserved_nodes_order_from_runtime_len = non_reserved_nodes_order_from_runtime.len();

    for i in (FREE_SEATS * session)..(FREE_SEATS * (session + 1)) {
        non_reserved.push(
            non_reserved_nodes_order_from_runtime
                [i as usize % non_reserved_nodes_order_from_runtime_len]
                .clone(),
        );
    }

    accounts_seeds_to_keys(&non_reserved)
        .iter()
        .map(|pair| AccountId::from(pair.public()))
        .collect()
}

fn get_authorities_for_session<C: AnyConnection>(connection: &C, session: u32) -> Vec<AccountId> {
    let session_period = get_session_period(connection);
    let first_block = session_period * session;

    let block = connection
        .as_connection()
        .get_block_hash(Some(first_block))
        .expect("Api call should succeed")
        .expect("Session already started so the first block should be present");

    connection
        .as_connection()
        .get_storage_value("Session", "Validators", Some(block))
        .expect("Api call should succeed")
        .expect("Authorities should always be present")
}

pub fn get_exposure<C: AnyConnection>(
    connection: &C,
    era: u32,
    account_id: &AccountId,
    block_hash: Option<H256>,
) -> Exposure<AccountId, u128> {
    connection
        .as_connection()
        .get_storage_double_map("Staking", "ErasStakers", era, account_id, block_hash)
        .expect("Failed to decode ErasStakers extrinsic!")
        .unwrap_or_else(|| panic!("Failed to obtain ErasStakers for era {}.", era))
}

// pub fn get_members_for_era<C: AnyConnection>(connection: &C, era: u32) {
//     let sessions_per_era = get_sessions_per_era(connection);
//     let session_period = get_session_period(connection);
//     let beggining_of_era_block = era * sessions_per_era * session_period;
//     let beggining_of_era_block_hash = get_block_hash(connection, beggining_of_era_block);

//     let validators: EraValidators<AccountId> = connection.as_connection().get_storage_value(
//         "Elections",
//         "CurrentEraValidators",
//         Some(beggining_of_era_block_hash),
//     );
// }

pub fn test_disable_node(config: &Config) -> anyhow::Result<()> {
    let root_connection: RootConnection =
        SignedConnection::new(&config.node, get_sudo_key(config)).into();
    // let current_era = wait_for_second_era(&root_connection);
    // wait_for_next_era(&root_connection)?;
    // let current_era = current_era + 1;

    let reserved_members: Vec<_> = get_reserved_members(config)
        .iter()
        .map(|pair| AccountId::from(pair.public()))
        .collect();
    let non_reserved_members: Vec<_> = get_non_reserved_members(config)
        .iter()
        .map(|pair| AccountId::from(pair.public()))
        .collect();

    let controller_connection =
        SignedConnection::new(&config.node, config.node_keys().controller_key);

    change_validators(
        &root_connection,
        Some(reserved_members.clone()),
        Some(non_reserved_members.clone()),
        Some(4),
        XtStatus::Finalized,
    );

    disable_validator(&controller_connection);

    let session_period = get_session_period(&root_connection);
    let sessions_per_era = get_sessions_per_era(&root_connection);
    // If during era 0 we request a controller to be a validator, it becomes one
    // for era 1, and payouts can be collected once era 1 ends,
    // so we want to start the test from era 2.
    // TODO these calculations are ugly and can't be used for the ForceNewEra tests
    let next_era_index = wait_for_full_era_completion(&root_connection)
        .expect("should be able to see era completion");
    let next_era_block = next_era_index * session_period * sessions_per_era;
    let next_era_block_hash = get_block_hash(&controller_connection, next_era_block);
    let era = get_era(&root_connection, Some(next_era_block_hash));

    let current_session = get_session(&controller_connection, Some(next_era_block_hash));

    let next_era_block = next_era_block + sessions_per_era * session_period;
    wait_for_finalized_block(&root_connection, next_era_block).expect("next era should start");
    let next_era_block_hash = get_block_hash(&controller_connection, next_era_block);

    let era_end_session = get_session(&controller_connection, Some(next_era_block_hash));

    let max_difference = 0.001;
    for session in (current_session..era_end_session) {
        let non_reserved_for_session = get_non_reserved_members_for_session(config, session);
        let non_reserved_bench = non_reserved_members
            .iter()
            .filter(|account_id| !non_reserved_for_session.contains(*account_id))
            .cloned()
            .collect::<Vec<_>>();

        // TODO we just disabled the first node (TODO ehhh, really?)
        let members: Vec<AccountId> = reserved_members
            .iter()
            .skip(1)
            .chain(non_reserved_for_session.iter())
            .cloned()
            .collect();
        let members_bench = non_reserved_bench;

        let test_result = test_points_and_payouts(
            &controller_connection,
            session,
            era,
            members,
            members_bench,
            max_difference,
        );
        if test_result.is_err() {
            return test_result;
        }
    }
    Ok(())
}

// fn get_sessions_for_era<C: AnyConnection>(connection: &C, era: u32) -> Range<u32> {}

// fn points_and_payouts_for_era(config: &Config, era: u32) -> anyhow::Result<()> {}

pub fn points_and_payouts(config: &Config) -> anyhow::Result<()> {
    let node = &config.node;
    let accounts = get_validators_keys(config);
    let sender = accounts.first().expect("Using default accounts").to_owned();
    let connection = SignedConnection::new(node, sender);

    let sessions_per_era: u32 = get_sessions_per_era(&connection);

    let session = 2 * sessions_per_era + 1;
    let era = session / sessions_per_era;

    let reserved_members: Vec<_> = get_reserved_members(config)
        .iter()
        .map(|pair| AccountId::from(pair.public()))
        .collect();

    reserved_members
        .iter()
        .for_each(|account_id| info!("Reserved member: {}", account_id));

    let non_reserved_members: Vec<_> = get_non_reserved_members(config)
        .iter()
        .map(|pair| AccountId::from(pair.public()))
        .collect();

    let non_reserved_for_session = get_non_reserved_members_for_session(config, session);
    non_reserved_for_session
        .iter()
        .for_each(|account_id| info!("Non-reserved member in committee: {}", account_id));

    let non_reserved_bench = non_reserved_members
        .into_iter()
        .filter(|account_id| !non_reserved_for_session.contains(account_id))
        .collect::<Vec<_>>();
    non_reserved_bench
        .iter()
        .for_each(|account_id| info!("Non-reserved member on bench: {}", account_id));

    let epsilon = 0.05;

    test_points_and_payouts(
        &connection,
        session,
        era,
        reserved_members.into_iter().chain(non_reserved_for_session),
        non_reserved_bench,
        epsilon,
    )
}

pub fn test_points_and_payouts(
    connection: &SignedConnection,
    session: u32,
    era: u32,
    members: impl IntoIterator<Item = AccountId>,
    members_bench: impl IntoIterator<Item = AccountId>,
    max_relative_difference: f64,
) -> anyhow::Result<()> {
    let session_period = get_session_period(connection);

    info!("[+] Era: {} | session: {}", era, session);

    let beggining_of_session_block = session * session_period;
    let end_of_session_block = beggining_of_session_block + session_period;
    info!("Waiting for block: {}", end_of_session_block);
    wait_for_finalized_block(connection, end_of_session_block)?;

    let beggining_of_session_block_hash = get_block_hash(connection, beggining_of_session_block);
    let end_of_session_block_hash = get_block_hash(connection, end_of_session_block);
    let before_end_of_session_block_hash = get_block_hash(connection, end_of_session_block - 1);
    info!("End-of-session block hash: {}.", end_of_session_block_hash);

    let members_per_session: u32 = connection
        .as_connection()
        .get_storage_value(
            "Elections",
            "CommitteeSize",
            Some(beggining_of_session_block_hash),
        )
        .expect("Failed to decode CommitteeSize extrinsic!")
        .unwrap_or_else(|| panic!("Failed to obtain CommitteeSize for session {}.", session));

    info!("Members per session: {}", members_per_session);

    let blocks_to_produce_per_session = session_period / members_per_session;
    info!(
        "Blocks to produce per session: {} - session period {}",
        blocks_to_produce_per_session, session_period
    );

    // get points stored by the Staking pallet
    let validator_reward_points_current_era =
        get_era_reward_points(connection, era, Some(end_of_session_block_hash)).individual;

    let validator_reward_points_previous_session =
        get_era_reward_points_result(connection, era, Some(beggining_of_session_block_hash))
            .expect("should be able to retrieve EraRewardPoints")
            .unwrap_or(EraRewardPoints::default())
            .individual;

    let validator_reward_points_current_session: BTreeMap<AccountId, RewardPoint> =
        validator_reward_points_current_era
            .clone()
            .into_iter()
            .map(|(account_id, reward_points)| {
                let reward_points_previous_session = validator_reward_points_previous_session
                    .get(&account_id)
                    .unwrap_or(&0);
                let reward_points_current = reward_points - reward_points_previous_session;

                info!(
                    "In session {} validator {} accumulated {}.",
                    session, account_id, reward_points
                );
                (account_id, reward_points_current)
            })
            .collect();

    let members_uptime = members.into_iter().map(|account_id| {
        (
            account_id.clone(),
            get_node_performance(
                &connection,
                account_id,
                session,
                before_end_of_session_block_hash,
                blocks_to_produce_per_session,
            ),
        )
    });

    let members_bench_uptime = members_bench
        .into_iter()
        .map(|account_id| (account_id, 1.0));

    let mut total_exposure = 0;
    let uptime_and_exposure: Vec<(AccountId, f64, u128)> = members_uptime
        .chain(members_bench_uptime)
        .map(|(account_id, performance)| {
            let exposure =
                download_exposure(&connection, era, &account_id, end_of_session_block_hash);
            total_exposure += exposure;
            (account_id, performance, exposure)
        })
        .collect();
    info!("Total exposure {}", total_exposure);

    let adjusted_reward_points = uptime_and_exposure
        .into_iter()
        .map(|(account_id, performance, exposure)| {
            let exposure_scaling = exposure as f64 / total_exposure as f64;
            info!(
                "Validator {}, scaling factor {} / {}.",
                account_id, exposure, total_exposure
            );
            info!(
                "Validator {}, scaling factor {:?}.",
                account_id, exposure_scaling
            );

            // let scaled_points = (exposure_scaling * (performance * f64::from(MAX_REWARD))) as u32;
            let scaled_points = exposure_scaling * performance;
            info!(
                "Validator {}, adjusted reward points {}",
                account_id, scaled_points
            );
            (account_id, scaled_points)
        })
        .collect();

    check_rewards(
        adjusted_reward_points,
        validator_reward_points_current_session,
        max_relative_difference,
    )
}

fn get_session_period<C: AnyConnection>(connection: &C) -> u32 {
    connection
        .as_connection()
        .get_constant("Elections", "SessionPeriod")
        .expect("Failed to decode SessionPeriod extrinsic!")
}

fn check_rewards(
    validator_reward_points: BTreeMap<AccountId, f64>,
    retrieved_reward_points: BTreeMap<AccountId, u32>,
    max_relative_difference: f64,
) -> anyhow::Result<()> {
    let our_sum: f64 = validator_reward_points
        .iter()
        .map(|(_, reward)| reward)
        .sum();
    let retrieved_sum: u32 = retrieved_reward_points
        .iter()
        .map(|(_, reward)| reward)
        .sum();

    for (account, reward) in validator_reward_points {
        let retrieved_reward = *retrieved_reward_points.get(&account).expect(&format!(
            "missing account={} in retrieved collection of reward points",
            account
        ));

        info!(
            "retrieved reward for {} is {} - calculated reward is {}",
            account, retrieved_reward, reward
        );

        let reward_ratio = reward / our_sum;
        let retrieved_ratio = retrieved_reward as f64 / retrieved_sum as f64;

        info!(
            "reward_ratio: {}; retrieved_ratio: {}",
            reward_ratio, retrieved_ratio
        );
        assert!((reward_ratio - retrieved_ratio).abs() <= max_relative_difference);
    }

    Ok(())
}

fn download_exposure(
    connection: &SignedConnection,
    era: u32,
    account_id: &AccountId,
    end_of_session_block_hash: H256,
) -> u128 {
    let exposure: Exposure<AccountId, u128> =
        get_exposure(connection, era, account_id, Some(end_of_session_block_hash));
    info!(
        "Validator {} has own exposure {}.",
        account_id, exposure.own
    );
    info!(
        "Validator {} has total exposure {}.",
        account_id, exposure.total
    );
    exposure.others.iter().for_each(|individual_exposure| {
        info!(
            "Validator {} has nominator {} exposure {}.",
            account_id, individual_exposure.who, individual_exposure.value
        )
    });
    exposure.total
}

fn get_node_performance(
    connection: &SignedConnection,
    account_id: AccountId,
    session: u32,
    before_end_of_session_block_hash: H256,
    blocks_to_produce_per_session: u32,
) -> f64 {
    let block_count: u32 = connection
        .as_connection()
        .get_storage_map(
            "Elections",
            "SessionValidatorBlockCount",
            account_id.clone(),
            Some(before_end_of_session_block_hash),
        )
        .expect("Failed to decode SessionValidatorBlockCount extrinsic!")
        .unwrap_or_else(|| {
            panic!(
                "Failed to obtain SessionValidatorBlockCount for session {}, validator {}, EOS block hash {}.",
                session, account_id, before_end_of_session_block_hash
            )
        });
    info!(
        "Block count for validator {} is {:?}, block hash is {}.",
        account_id, block_count, before_end_of_session_block_hash
    );
    let performance = block_count as f64 / blocks_to_produce_per_session as f64;
    info!("Validator {}, performance {:?}", account_id, performance);
    // NOTE following value allows us to make performance of a node to be bigger than 1.0
    let lenient_performance = match Perquintill::from_float(performance) >= LENIENT_THRESHOLD
        && blocks_to_produce_per_session >= block_count
    {
        true => 1.0,
        false => performance,
    };
    info!(
        "Validator {}, lenient performance {:?}",
        account_id, lenient_performance
    );
    info!(
        "Validator {}, lenient performance 2 {} / {}",
        account_id, block_count, blocks_to_produce_per_session
    );
    lenient_performance
}

fn get_block_hash<C: AnyConnection>(connection: &C, block_number: u32) -> H256 {
    connection
        .as_connection()
        .get_block_hash(Some(block_number))
        .expect("API call should have succeeded.")
        .unwrap_or_else(|| {
            panic!("Failed to obtain block hash for block {}.", block_number);
        })
}

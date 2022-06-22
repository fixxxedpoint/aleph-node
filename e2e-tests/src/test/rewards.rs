use crate::{
    accounts::{accounts_seeds_to_keys, get_sudo_key, get_validators_keys, get_validators_seeds},
    Config,
};
use aleph_client::{
    change_validators, get_current_session, get_era_reward_points, wait_for_finalized_block,
    wait_for_full_era_completion, AnyConnection, Header, KeyPair, RewardPoint, RootConnection,
    SignedConnection,
};
use pallet_staking::Exposure;
use primitives::{LENIENT_THRESHOLD, MAX_REWARD};
use sp_core::{Pair, H256};
use std::collections::BTreeMap;
use substrate_api_client::{AccountId, XtStatus};

use frame_election_provider_support::sp_arithmetic::Perquintill;
use log::info;

const ERAS: u32 = 100;

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
    const SESSION_PERIOD: u32 = 30;
    let first_block = SESSION_PERIOD * session;

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

pub fn points_and_payouts(config: &Config) -> anyhow::Result<()> {
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
        .iter()
        .filter(|account_id| !non_reserved_for_session.contains(account_id))
        .collect::<Vec<_>>();
    non_reserved_bench
        .iter()
        .for_each(|account_id| info!("Non-reserved member on bench: {}", account_id));

    assert_eq!(
        (reserved_members.len() + non_reserved_for_session.len()) as u32,
        members_per_session
    );

    let session = 1;
    let era = 1;

    test_points_and_payouts(
        config,
        session,
        era,
        reserved_members,
        reserved_members
            .into_iter()
            .chain(non_reserved_for_session.into_iter()),
        non_reserved_bench,
        epsilon,
    )
}

pub fn test_points_and_payouts(
    config: &Config,
    session: u32,
    era: u32,
    reserved_members: impl IntoIterator<Item = AccountId>,
    members: impl IntoIterator<Item = AccountId>,
    members_bench: impl IntoIterator<Item = AccountId>,
    epsilon: f64,
) -> anyhow::Result<()> {
    let node = &config.node;
    let accounts = get_validators_keys(config);
    let sender = accounts.first().expect("Using default accounts").to_owned();
    let connection = SignedConnection::new(node, sender);

    let session_period: u32 = connection
        .as_connection()
        .get_constant("Elections", "SessionPeriod")
        .expect("Failed to decode SessionPeriod extrinsic!");

    let sessions_per_era: u32 = connection
        .as_connection()
        .get_constant("Staking", "SessionsPerEra")
        .expect("Failed to decode SessionsPerEra extrinsic!");

    info!("[+] Era: {} | session: {}", era, session);
    info!("Sessions per era: {}", sessions_per_era);

    let beggining_of_session_block = session * sessior_period;
    let end_of_session_block = beggining_of_session_block + session_period;
    info!("Waiting for block: {}", end_of_session_block);
    wait_for_finalized_block(&connection, end_of_session_block)?;

    let beggining_of_session_block_hash = get_block_hash(&connection, beggining_of_session_block);
    let end_of_session_block_hash = get_block_hash(&connection, end_of_session_block);
    info!("End-of-session block hash: {}.", end_of_session_block_hash);

    let before_end_of_session_block_hash = get_block_hash(&connection, end_of_session_block - 1);

    let members_per_session: u32 = connection
        .as_connection()
        .get_storage_value(
            "Elections",
            "CommitteeSize",
            Some(end_of_session_block_hash),
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
        get_era_reward_points(&connection, era, Some(end_of_session_block_hash)).individual;

    let validator_reward_points_previous_session =
        get_era_reward_points(&connection, era, Some(beggining_of_session_block_hash)).individual;

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
                )(account_id, reward_points_current)
            })
            .collect();

    let total_exposure: u128 = validator_exposures
        .iter()
        .map(|(_, exposure)| exposure)
        .sum();

    info!("Total exposure {}", total_exposure);

    let members_performance = members.into_iter().map(|account_id| {
        (
            account_id.clone(),
            get_node_performance(
                &connection,
                account_id,
                session,
                before_end_of_session_block_hash,
                blocks_to_produce_per_session,
                sessions_per_era,
            ),
        )
    });

    let members_bench_performance = members_bench.into_iter().map(|account_id| {
        (
            account_id,
            Perquintill::from_rational(1, sessions_per_era as u64),
        )
    });

    let adjusted_reward_points: Vec<(AccountId, u128)> = members_performance
        .iter()
        .chain(members_bench_performance.iter())
        .cloned()
        .map(|account_id, performance| {
            let exposure =
                download_exposure(&connection, era, account_id, end_of_session_block_hash);

            let scaling_factor = Perquintill::from_rational(exposure, total_exposure);
            info!(
                "Validator {}, scaling factor {} / {}.",
                account_id, exposure, total_exposure
            );
            info!(
                "Validator {}, scaling factor {:?}.",
                account_id, scaling_factor
            );

            let scaled_points = (scaling_factor * (performance * u64::from(MAX_REWARD))) as u32;
            info!(
                "Validator {}, adjusted reward points {}",
                account_id, scaled_points
            );
            (account_id, scaled_points)
        })
        .collect();

    assert_eq!(
        validator_reward_points_current_session,
        adjusted_reward_points
    );
    check_rewards(
        validator_reward_points_current_era,
        adjusted_reward_points,
        epsilon,
    )
}

fn check_rewards(
    validator_reward_points: BTreeMap<AccountId, u32>,
    retrieved_reward_points: BTreeMap<AccountId, u32>,
    epsilon: f64,
) -> anyhow::Result<()> {
    let computed_sum = validator_reward_points.iter().unzip().1.sum();
    let retrieved_sum = retrieved_reward_points.iter().unzip().1.sum();

    for (account, reward) in validator_reward_points {
        let retrieved_reward = retrieved_reward_points.get(&account).expect(&format!(
            "missing account={} in retrieved collection of reward points",
            account
        ));

        let reward_ratio = reward as f64 / computed_sum as f64;
        let retrieved_ratio = retrieved_reward as f64 / retrieved_sum as f64;

        assert!((reward_ratio - retrieved_ratio).abs() <= epsilon);
    }

    Ok(())
}

fn download_exposure(
    connection: &SignedConnection,
    era: u32,
    account_id: AccountId,
    end_of_session_block_hash: H256,
) -> u128 {
    let exposure: Exposure<AccountId, u128> = get_exposure(
        connection,
        era,
        &account_id,
        Some(end_of_session_block_hash),
    );
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
    sessions_per_era: u32,
) -> Perquintill {
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
    let performance =
        Perquintill::from_rational(block_count as u64, blocks_to_produce_per_session as u64);
    info!("Validator {}, performance {:?}", account_id, performance);
    // TODO zamienilem > na >=
    let lenient_performance =
        match performance >= LENIENT_THRESHOLD && blocks_to_produce_per_session >= block_count {
            true => Perquintill::from_rational(1, sessions_per_era as u64),
            false => Perquintill::from_rational(
                block_count as u64,
                blocks_to_produce_per_session as u64 * sessions_per_era as u64,
            ),
        };
    info!(
        "Validator {}, lenient performance {:?}",
        account_id, lenient_performance
    );
    info!(
        "Validator {}, lenient performance 2 {} / {}",
        account_id, block_count, blocks_to_produce_per_session
    );
    // Perquintill::from_rational(block_count as u64, blocks_to_produce_per_sessionas as u64)
    lenient_performance
}

fn get_block_hash(connection: &SignedConnection, block_number: u32) {
    connection
        .as_connection()
        .get_block_hash(Some(block_number))
        .expect("API call should have succeeded.")
        .unwrap_or_else(|| {
            panic!("Failed to obtain block hash for block {}.", block_number);
        })
}

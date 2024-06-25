use std::sync::{atomic::AtomicBool, Arc};

use libp2p::core::transport::Transport;
use log::error;
use sc_client_api::Backend;
use sc_network::{
    config::{NetworkConfiguration, ProtocolId},
    error::Error as NetworkError,
    NetworkService,
};
use sc_network_sync::SyncingService;
use sc_network_transactions::TransactionsHandlerController;
use sc_rpc::system::Request as RpcRequest;
use sc_service::SpawnTaskHandle;
use sc_transaction_pool_api::TransactionPool;
use sc_utils::mpsc::TracingUnboundedSender;
use sp_runtime::traits::{Block, Header};
use substrate_prometheus_endpoint::Registry;

use crate::{
    network::{
        base_protocol::{setup as setup_base_protocol, Service as BaseProtocolService},
        LOG_TARGET,
    },
    BlockHash, BlockNumber, ClientForAleph, ProtocolNetwork, RateLimiterConfig,
};

mod base;
mod own_protocols;
mod rpc;
mod transactions;
pub mod transport;

use base::network as base_network;
use own_protocols::Networks;
use rpc::spawn_rpc_service;
use transactions::spawn_transaction_handler;

use self::transport::RateLimitedStreamMuxer;

const SPAWN_CATEGORY: Option<&str> = Some("networking");

/// Components created when spawning the network.
pub struct NetworkOutput<TP: TransactionPool + 'static> {
    pub network: Arc<NetworkService<TP::Block, TP::Hash>>,
    pub authentication_network: ProtocolNetwork,
    pub block_sync_network: ProtocolNetwork,
    // names chosen for compatibility with SpawnTaskParams, get better ones if we ever stop using that
    pub sync_service: Arc<SyncingService<TP::Block>>,
    pub tx_handler_controller: TransactionsHandlerController<TP::Hash>,
    pub system_rpc_tx: TracingUnboundedSender<RpcRequest<TP::Block>>,
}

pub struct SubstrateNetworkConfig {
    /// Maximum bit-rate per node in bytes per second of the substrate network (shared by sync, gossip, etc.).
    pub substrate_bit_rate_per_connection: usize,
}

impl From<RateLimiterConfig> for SubstrateNetworkConfig {
    fn from(value: RateLimiterConfig) -> Self {
        Self {
            substrate_bit_rate_per_connection: value.substrate_bit_rate_per_connection,
        }
    }
}

/// Start everything necessary to run the inter-node network and return the interfaces for it.
/// This includes everything in the base network, the base protocol service, and services for handling transactions and RPCs.
pub fn network<TP, BE, C>(
    network_config: &NetworkConfiguration,
    transport_config: SubstrateNetworkConfig,
    protocol_id: ProtocolId,
    client: Arc<C>,
    major_sync: Arc<AtomicBool>,
    transaction_pool: Arc<TP>,
    spawn_handle: &SpawnTaskHandle,
    metrics_registry: Option<Registry>,
) -> Result<NetworkOutput<TP>, NetworkError>
where
    TP: TransactionPool<Hash = BlockHash> + 'static,
    TP::Block: Block<Hash = BlockHash>,
    <TP::Block as Block>::Header: Header<Number = BlockNumber>,
    BE: Backend<TP::Block>,
    C: ClientForAleph<TP::Block, BE>,
{
    let genesis_hash = client
        .hash(0)
        .ok()
        .flatten()
        .expect("Genesis block exists.");
    let (base_protocol_config, events_from_network) =
        setup_base_protocol::<TP::Block>(genesis_hash);

    let rate_per_connection = transport_config.substrate_bit_rate_per_connection;
    let transport_builder = move |config: sc_network::transport::NetworkConfig| {
        let default_transport = sc_network::transport::build_transport(
            config.keypair,
            config.memory_only,
            config.muxer_window_size,
            config.muxer_maximum_buffer_size,
        );
        default_transport.map(move |(peer_id, stream_muxer), _| {
            (
                peer_id,
                RateLimitedStreamMuxer::new(stream_muxer, rate_per_connection),
            )
        })
    };

    let (
        network,
        Networks {
            block_sync_network,
            authentication_network,
        },
        transaction_prototype,
    ) = base_network(
        network_config,
        transport_builder,
        protocol_id,
        client.clone(),
        spawn_handle,
        base_protocol_config,
        metrics_registry.clone(),
    )?;
    let protocol_names = vec![authentication_network.name(), block_sync_network.name()];
    let (base_service, syncing_service) = BaseProtocolService::new(
        major_sync,
        genesis_hash,
        network_config,
        protocol_names,
        network.clone(),
        events_from_network,
    );
    spawn_handle.spawn("base-protocol", SPAWN_CATEGORY, async move {
        if let Err(e) = base_service.run().await {
            error!(target: LOG_TARGET, "Base protocol service finished with error: {e}.");
        }
    });
    let transaction_interface = spawn_transaction_handler(
        network.clone(),
        syncing_service.clone(),
        client.clone(),
        transaction_pool,
        transaction_prototype,
        metrics_registry.as_ref(),
        spawn_handle,
    )?;
    let rpc_interface = spawn_rpc_service(
        network.clone(),
        syncing_service.clone(),
        client,
        spawn_handle,
    );
    Ok(NetworkOutput {
        network,
        block_sync_network,
        authentication_network,
        sync_service: syncing_service,
        tx_handler_controller: transaction_interface,
        system_rpc_tx: rpc_interface,
    })
}

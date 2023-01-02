#![feature(auto_traits)]
#![feature(negative_impls)]

extern crate core;

pub use contract_transcode;
pub use subxt::ext::sp_core::Pair;
use subxt::{
    ext::sp_core::{ed25519, sr25519, H256},
    tx::PairSigner,
    OnlineClient, PolkadotConfig,
};

use crate::api::runtime_types::aleph_runtime::RuntimeCall as Call;
// generated by running `subxt codegen --derive Clone Debug Eq PartialEq | rustfmt --edition=2021 > src/aleph_zero.rs`
#[allow(clippy::all)]
mod aleph_zero;
mod connections;
pub mod contract;
pub mod pallets;
mod runtime_types;
pub mod utility;
pub mod waiting;

pub use aleph_zero::api;
pub use runtime_types::*;

pub type AlephConfig = PolkadotConfig;
pub type AlephKeyPair = ed25519::Pair;
pub type RawKeyPair = sr25519::Pair;
pub type KeyPair = PairSigner<AlephConfig, sr25519::Pair>;
pub type AccountId = subxt::ext::sp_core::crypto::AccountId32;
pub type BlockHash = H256;

pub(crate) type SubxtClient = OnlineClient<AlephConfig>;

pub use connections::{
    Connection, ConnectionApi, RootConnection, SignedConnection, SignedConnectionApi, SudoCall,
};

#[derive(Copy, Clone)]
pub enum TxStatus {
    InBlock,
    Finalized,
    Submitted,
}

pub fn keypair_from_string(seed: &str) -> KeyPair {
    let pair = sr25519::Pair::from_string(seed, None).expect("Can't create pair from seed value");
    KeyPair::new(pair)
}

pub fn raw_keypair_from_string(seed: &str) -> RawKeyPair {
    sr25519::Pair::from_string(seed, None).expect("Can't create pair from seed value")
}

pub fn aleph_keypair_from_string(seed: &str) -> AlephKeyPair {
    ed25519::Pair::from_string(seed, None).expect("Can't create pair from seed value")
}

pub fn account_from_keypair<P>(keypair: &P) -> AccountId
where
    P: Pair,
    AccountId: From<<P as Pair>::Public>,
{
    AccountId::from(keypair.public())
}

use std::marker::PhantomData;

use aleph_bft::{Network as AlephNetwork, NetworkData as AlephNetworkData, SignatureSet};
use log::warn;
use sp_runtime::traits::Block;

use crate::{
    crypto::Signature,
    data_io::{AlephData, AlephNetworkMessage},
    network::{Data, DataNetwork},
    Hasher,
};

pub type NetworkData<B> =
    AlephNetworkData<Hasher, AlephData<B>, Signature, SignatureSet<Signature>>;

impl<B: Block> AlephNetworkMessage<B> for NetworkData<B> {
    fn included_data(&self) -> Vec<AlephData<B>> {
        self.included_data()
    }
}

/// A wrapper needed only because of type system theoretical constraints. Sadness.
pub struct NetworkWrapper<D: Data, DN: DataNetwork<D>> {
    inner: DN,
    _phantom: PhantomData<D>,
}

impl<D: Data, DN: DataNetwork<D>> From<DN> for NetworkWrapper<D, DN> {
    fn from(inner: DN) -> Self {
        NetworkWrapper {
            inner,
            _phantom: PhantomData,
        }
    }
}

#[async_trait::async_trait]
impl<D: Data, DN: DataNetwork<D>> AlephNetwork<D> for NetworkWrapper<D, DN> {
    fn send(&self, data: D, recipient: aleph_bft::Recipient) {
        if self.inner.send(data, recipient).is_err() {
            warn!(target: "aleph-network", "Error sending an AlephBFT message to the network.");
        }
    }

    async fn next_event(&mut self) -> Option<D> {
        self.inner.next().await
    }
}

pub struct GuardedNetworkWrapper<D: Data, DN: DataNetwork<D>>(
    std::sync::Arc<futures::lock::Mutex<NetworkWrapper<D, DN>>>,
);

impl<D: Data, DN: DataNetwork<D>> GuardedNetworkWrapper<D, DN> {
    pub fn new(network: std::sync::Arc<futures::lock::Mutex<NetworkWrapper<D, DN>>>) -> Self {
        // Self(std::sync::Arc::new(futures::lock::Mutex::new(network)))
        Self(network)
    }
}

#[async_trait::async_trait]
impl<D: Data, DN: DataNetwork<D>> AlephNetwork<D> for GuardedNetworkWrapper<D, DN> {
    fn send(&self, data: D, recipient: aleph_bft::Recipient) {
        self.0.try_lock().unwrap().send(data, recipient);
    }

    async fn next_event(&mut self) -> Option<D> {
        self.0.lock().await.next_event().await
    }
}

// #[async_trait::async_trait]
// impl<D: Data, DN: DataNetwork<D>, T: DerefMut<Target = NetworkWrapper<D, DN>>> AlephNetwork<D>
//     for T
// {
//     fn send(&self, data: D, recipient: aleph_bft::Recipient) {
//         if self.deref().inner.send(data, recipient).is_err() {
//             warn!(target: "aleph-network", "Error sending an AlephBFT message to the network.");
//         }
//     }

//     async fn next_event(&mut self) -> Option<D> {
//         self.inner.next().await
//     }
// }

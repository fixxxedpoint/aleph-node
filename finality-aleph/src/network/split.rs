// WARNING: A lot of the code below is duplicated and cannot be easily deduplicated within the Rust
// typesystem (perhaps somewhat with macros?). Be very careful to change all the occurences if you
// are modyfing this file.
use std::{marker::PhantomData, sync::Arc};

use aleph_bft::Recipient;
use codec::{Decode, Encode};
use futures::channel::mpsc;
use log::{trace, warn};
use tokio::sync::Mutex;

use crate::network::{ComponentNetwork, Data, ReceiverComponent, SendError, SenderComponent};

/// Used for routing data through split networks.
#[derive(Clone, Encode, Decode)]
pub enum Split<LeftData: Data, RightData: Data> {
    Left(LeftData),
    Right(RightData),
}

trait Convert<A, B> {
    fn convert(a: A) -> B;
}

#[derive(Clone)]
struct GenericSender<
    LeftData: Data,
    RightData: Data,
    S: SenderComponent<Split<LeftData, RightData>>,
    MyData,
    // Convert: Fn(MyData) -> Split<LeftData, RightData>,
    Convert: Convert<MyData, Split<LeftData, RightData>>,
> {
    sender: S,
    phantom: PhantomData<(LeftData, RightData, MyData, Convert)>,
}

struct LeftSender<LeftData: Data, RightData: Data, S: SenderComponent<Split<LeftData, RightData>>>
{}

impl<LeftData: Data, RightData: Data, S: SenderComponent<Split<LeftData, RightData>>, MyData>
    GenericSender<LeftData, RightData, S, LeftData>
{
    fn wrap_data(data: LeftData) -> Split<LeftData, RightData> {
        Split::Left(data)
    }
}

impl<LeftData: Data, RightData: Data, S: SenderComponent<Split<LeftData, RightData>>, MyData>
    GenericSender<LeftData, RightData, S, RightData>
{
    fn wrap_data(data: RightData) -> Split<LeftData, RightData> {
        Split::Right(data)
    }
}

impl<LeftData: Data, RightData: Data, S: SenderComponent<Split<LeftData, RightData>>>
    SenderComponent<LeftData> for GenericSender<LeftData, RightData, S, LeftData>
{
    fn send(&self, data: LeftData, recipient: Recipient) -> Result<(), SendError> {
        self.sender.send(Split::Left(data), recipient)
    }
}

impl<LeftData: Data, RightData: Data, S: SenderComponent<Split<LeftData, RightData>>>
    SenderComponent<RightData> for GenericSender<LeftData, RightData, S, RightData>
{
    fn send(&self, data: LeftData, recipient: Recipient) -> Result<(), SendError> {
        self.sender.send(Split::Right(data), recipient)
    }
}

#[derive(Clone)]
struct LeftSender<LeftData: Data, RightData: Data, S: SenderComponent<Split<LeftData, RightData>>> {
    sender: S,
    phantom: PhantomData<(LeftData, RightData)>,
}

impl<LeftData: Data, RightData: Data, S: SenderComponent<Split<LeftData, RightData>>>
    SenderComponent<LeftData> for LeftSender<LeftData, RightData, S>
{
    fn send(&self, data: LeftData, recipient: Recipient) -> Result<(), SendError> {
        self.sender.send(Split::Left(data), recipient)
    }
}

#[derive(Clone)]
struct RightSender<LeftData: Data, RightData: Data, S: SenderComponent<Split<LeftData, RightData>>>
{
    sender: S,
    phantom: PhantomData<(LeftData, RightData)>,
}

impl<LeftData: Data, RightData: Data, S: SenderComponent<Split<LeftData, RightData>>>
    SenderComponent<RightData> for RightSender<LeftData, RightData, S>
{
    fn send(&self, data: RightData, recipient: Recipient) -> Result<(), SendError> {
        self.sender.send(Split::Right(data), recipient)
    }
}

struct GenericReceiver<
    LeftData: Data,
    RightData: Data,
    R: ReceiverComponent<Split<LeftData, RightData>>,
    TranslatedData: Data,
> {
    receiver: Arc<Mutex<R>>,
    translated_receiver: mpsc::UnboundedReceiver<TranslatedData>,
    left_sender: mpsc::UnboundedSender<LeftData>,
    right_sender: mpsc::UnboundedSender<RightData>,
    name: String,
}

// impl<
//         LeftData: Data,
//         RightData: Data,
//         R: ReceiverComponent<Split<LeftData, RightData>>,
//         TranslatedData: Data,
//     > GenericReceiver<LeftData, RightData, R, TranslatedData>
// {
//     fn new(receiver: Arc<Mutex<R>>, left_sender: mpsc::UnboundedSender<LeftData) {
//     }
// }

#[async_trait::async_trait]
impl<
        LeftData: Data,
        RightData: Data,
        R: ReceiverComponent<Split<LeftData, RightData>>,
        MyData: Data,
    > ReceiverComponent<MyData> for GenericReceiver<LeftData, RightData, R, MyData>
{
    async fn next(&mut self) -> Option<MyData> {
        loop {
            tokio::select! {
                data = self.translated_receiver.next() => {
                    return data;
                },
                should_go_on = forward_or_wait(&self.receiver, &self.left_sender, &self.right_sender, self.name.to_string()) => {
                    if !should_go_on {
                        return None;
                    }
                },
            }
        }
    }
}

type LeftReceiver<
    LeftData: Data,
    RightData: Data,
    R: ReceiverComponent<Split<LeftData, RightData>>,
> = GenericReceiver<LeftData, RightData, R, LeftData>;

type RightReceiver<
    LeftData: Data,
    RightData: Data,
    R: ReceiverComponent<Split<LeftData, RightData>>,
> = GenericReceiver<LeftData, RightData, R, RightData>;

// struct LeftReceiver<
//     LeftData: Data,
//     RightData: Data,
//     R: ReceiverComponent<Split<LeftData, RightData>>,
// > {
//     receiver: Arc<Mutex<R>>,
//     translated_receiver: mpsc::UnboundedReceiver<LeftData>,
//     left_sender: mpsc::UnboundedSender<LeftData>,
//     right_sender: mpsc::UnboundedSender<RightData>,
//     name: &'static str,
// }

// struct RightReceiver<
//     LeftData: Data,
//     RightData: Data,
//     R: ReceiverComponent<Split<LeftData, RightData>>,
// > {
//     receiver: Arc<Mutex<R>>,
//     translated_receiver: mpsc::UnboundedReceiver<RightData>,
//     left_sender: mpsc::UnboundedSender<LeftData>,
//     right_sender: mpsc::UnboundedSender<RightData>,
//     name: &'static str,
// }

async fn forward_or_wait<
    LeftData: Data,
    RightData: Data,
    R: ReceiverComponent<Split<LeftData, RightData>>,
>(
    receiver: &Arc<Mutex<R>>,
    left_sender: &mpsc::UnboundedSender<LeftData>,
    right_sender: &mpsc::UnboundedSender<RightData>,
    name: String,
) -> bool {
    match receiver.lock().await.next().await {
        Some(Split::Left(data)) => {
            if left_sender.unbounded_send(data).is_err() {
                warn!(target: "aleph-network", "Failed send despite controlling receiver, this shouldn't've happened. left sender :: {}", name);
            }
            true
        }
        Some(Split::Right(data)) => {
            if right_sender.unbounded_send(data).is_err() {
                warn!(target: "aleph-network", "Failed send despite controlling receiver, this shouldn't've happened. right sender :: {}", name);
            }
            true
        }
        None => {
            trace!(target: "aleph-network", "Split data channel ended");
            left_sender.close_channel();
            right_sender.close_channel();
            false
        }
    }
}

// #[async_trait::async_trait]
// impl<LeftData: Data, RightData: Data, R: ReceiverComponent<Split<LeftData, RightData>>>
//     ReceiverComponent<LeftData> for LeftReceiver<LeftData, RightData, R>
// {
//     async fn next(&mut self) -> Option<LeftData> {
//         loop {
//             tokio::select! {
//                 data = self.translated_receiver.next() => {
//                     return data;
//                 },
//                 should_go_on = forward_or_wait(&self.receiver, &self.left_sender, &self.right_sender, self.name.to_string()) => {
//                     if !should_go_on {
//                         return None;
//                     }
//                 },
//             }
//         }
//     }
// }

// #[async_trait::async_trait]
// impl<LeftData: Data, RightData: Data, R: ReceiverComponent<Split<LeftData, RightData>>>
//     ReceiverComponent<RightData> for RightReceiver<LeftData, RightData, R>
// {
//     async fn next(&mut self) -> Option<RightData> {
//         loop {
//             tokio::select! {
//                 data = self.translated_receiver.next() => {
//                     return data;
//                 },
//                 should_go_on = forward_or_wait(&self.receiver, &self.left_sender, &self.right_sender, self.name.to_string()) => {
//                     if !should_go_on {
//                         return None;
//                     }
//                 },
//             }
//         }
//     }
// }

struct LeftNetwork<
    LeftData: Data,
    RightData: Data,
    S: SenderComponent<Split<LeftData, RightData>>,
    R: ReceiverComponent<Split<LeftData, RightData>>,
> {
    sender: LeftSender<LeftData, RightData, S>,
    receiver: Arc<Mutex<LeftReceiver<LeftData, RightData, R>>>,
    name: &'static str,
}

impl<
        LeftData: Data,
        RightData: Data,
        S: SenderComponent<Split<LeftData, RightData>>,
        R: ReceiverComponent<Split<LeftData, RightData>>,
    > ComponentNetwork<LeftData> for LeftNetwork<LeftData, RightData, S, R>
{
    type S = LeftSender<LeftData, RightData, S>;
    type R = LeftReceiver<LeftData, RightData, R>;
    fn sender(&self) -> &Self::S {
        &self.sender
    }
    fn receiver(&self) -> Arc<Mutex<Self::R>> {
        self.receiver.clone()
    }
}

struct RightNetwork<
    LeftData: Data,
    RightData: Data,
    S: SenderComponent<Split<LeftData, RightData>>,
    R: ReceiverComponent<Split<LeftData, RightData>>,
> {
    sender: RightSender<LeftData, RightData, S>,
    receiver: Arc<Mutex<RightReceiver<LeftData, RightData, R>>>,
    name: &'static str,
}

impl<
        LeftData: Data,
        RightData: Data,
        S: SenderComponent<Split<LeftData, RightData>>,
        R: ReceiverComponent<Split<LeftData, RightData>>,
    > ComponentNetwork<RightData> for RightNetwork<LeftData, RightData, S, R>
{
    type S = RightSender<LeftData, RightData, S>;
    type R = RightReceiver<LeftData, RightData, R>;
    fn sender(&self) -> &Self::S {
        &self.sender
    }
    fn receiver(&self) -> Arc<Mutex<Self::R>> {
        self.receiver.clone()
    }
}

fn split_sender<LeftData: Data, RightData: Data, S: SenderComponent<Split<LeftData, RightData>>>(
    sender: &S,
) -> (
    LeftSender<LeftData, RightData, S>,
    RightSender<LeftData, RightData, S>,
) {
    (
        LeftSender {
            sender: sender.clone(),
            phantom: PhantomData,
        },
        RightSender {
            sender: sender.clone(),
            phantom: PhantomData,
        },
    )
}

fn split_receiver<
    LeftData: Data,
    RightData: Data,
    R: ReceiverComponent<Split<LeftData, RightData>>,
>(
    receiver: Arc<Mutex<R>>,
    left_name: &'static str,
    right_name: &'static str,
) -> (
    LeftReceiver<LeftData, RightData, R>,
    RightReceiver<LeftData, RightData, R>,
) {
    let (left_sender, left_receiver) = mpsc::unbounded();
    let (right_sender, right_receiver) = mpsc::unbounded();
    let left_receiver = GenericReceiver {
        receiver: receiver.clone(),
        translated_receiver: left_receiver,
        left_sender: left_sender.clone(),
        right_sender: right_sender.clone(),
        name: left_name.to_string(),
    };
    let right_receiver = GenericReceiver {
        receiver,
        translated_receiver: right_receiver,
        left_sender,
        right_sender,
        name: right_name.to_string(),
    };

    (left_receiver, right_receiver)
}

/// Split a single component network into two separate ones. This way multiple components can send
/// data to the same underlying session not knowing what types of data the other ones use.
///
/// Internally the returned networks compete for data returned by their parent network when
/// `next()` is polled, and unpack it to two separate channels. At the same time each polls
/// the end of those channels which contains the type that it is supposed to return.
///
/// The main example for now is creating an `aleph_bft::Network` and a separate one for accumulating
/// signatures for justifications.
pub fn split<LeftData: Data, RightData: Data, CN: ComponentNetwork<Split<LeftData, RightData>>>(
    network: CN,
    left_name: &'static str,
    right_name: &'static str,
) -> (
    impl ComponentNetwork<LeftData>,
    impl ComponentNetwork<RightData>,
) {
    let (left_sender, right_sender) = split_sender(network.sender());
    let (left_receiver, right_receiver) = split_receiver(network.receiver(), left_name, right_name);
    (
        LeftNetwork {
            sender: left_sender,
            receiver: Arc::new(Mutex::new(left_receiver)),
            name: left_name,
        },
        RightNetwork {
            sender: right_sender,
            receiver: Arc::new(Mutex::new(right_receiver)),
            name: right_name,
        },
    )
}

use std::{
    fmt::{Display, Error as FmtError, Formatter},
    pin::Pin,
};

use futures::{
    stream::{poll_fn, unfold, FusedStream, Pending},
    FutureExt, Stream, StreamExt,
};
use sc_client_api::client::{FinalityNotifications, ImportNotifications};
use tokio::select;

use crate::{
    aleph_primitives::{Block, Header},
    block::{ChainStatusNotification, ChainStatusNotifier},
};

/// What can go wrong when waiting for next chain status notification.
#[derive(Debug)]
pub enum Error {
    JustificationStreamClosed,
    ImportStreamClosed,
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), FmtError> {
        use Error::*;
        match self {
            JustificationStreamClosed => {
                write!(f, "finalization notification stream has ended")
            }
            ImportStreamClosed => {
                write!(f, "import notification stream has ended")
            }
        }
    }
}

/// Substrate specific implementation of `ChainStatusNotifier`.
pub struct SubstrateChainStatusNotifier {
    finality_notifications: FinalityNotifications<Block>,
    import_notifications: ImportNotifications<Block>,
}

impl SubstrateChainStatusNotifier {
    pub fn new(
        finality_notifications: FinalityNotifications<Block>,
        import_notifications: ImportNotifications<Block>,
    ) -> Self {
        Self {
            finality_notifications,
            import_notifications,
        }
    }

    // pub fn into_stream(mut self) -> impl Stream<Item = ChainStatusNotification<Header>> {
    //     let mut next = if false {
    //         Some(<Self as ChainStatusNotifier<Header>>::next(&mut self))
    //     } else {
    //         None
    //     };
    //     let callback = |cx: &mut std::task::Context<'_>| {
    //         // next = Some(<Self as ChainStatusNotifier<Header>>::next(&mut self));
    //         match next.as_mut() {
    //             Some(next) => match next.poll_unpin(cx) {
    //                 std::task::Poll::Ready(_) => todo!(),
    //                 std::task::Poll::Pending => todo!(),
    //             },
    //             None => {
    //                 let task = <Self as ChainStatusNotifier<Header>>::next(&mut self);
    //                 let result = task.poll_unpin(cx);
    //                 match result {
    //                     std::task::Poll::Ready(item) => match item {
    //                         Ok(item) => std::task::Poll::Ready(Some(item)),
    //                         Err(_) => std::task::Poll::Ready(None),
    //                     },
    //                     std::task::Poll::Pending => {
    //                         next = Some(task);
    //                         std::task::Poll::Pending
    //                     }
    //                 }
    //                 // next = Some(task);
    //                 // std::task::Poll::Pending
    //             }
    //         }
    //     };
    //     poll_fn(callback)
    // }
}

#[async_trait::async_trait]
impl ChainStatusNotifier<Header> for SubstrateChainStatusNotifier {
    type Error = Error;

    async fn next(&mut self) -> Result<ChainStatusNotification<Header>, Self::Error> {
        select! {
            maybe_block = self.finality_notifications.next(), if !self.finality_notifications.is_terminated() => {
                maybe_block
                    .map(|block| ChainStatusNotification::BlockFinalized(block.header))
                    .ok_or(Error::JustificationStreamClosed)
            },
            maybe_block = self.import_notifications.next(), if !self.import_notifications.is_terminated() => {
                maybe_block
                .map(|block| ChainStatusNotification::BlockImported(block.header))
                .ok_or(Error::ImportStreamClosed)
            }
        }
    }
}

// impl Stream for SubstrateChainStatusNotifier {
//     type Item = ChainStatusNotification<Header>;

//     fn poll_next(
//         self: std::pin::Pin<&mut Self>,
//         cx: &mut std::task::Context<'_>,
//     ) -> std::task::Poll<Option<Self::Item>> {
//         let inner = self.get_mut();
//         match <Self as ChainStatusNotifier<_>>::next(inner).poll_unpin(cx) {
//             std::task::Poll::Ready(next) => {}
//             std::task::Poll::Pending => std::task::Poll::Pending,
//         }
//     }
// }

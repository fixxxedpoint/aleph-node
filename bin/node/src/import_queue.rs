use futures::channel::mpsc::{unbounded, UnboundedSender};
use sc_consensus::ImportQueue;
use sp_runtime::traits::Block;

pub struct SplitImportQueue<B: Block> {
    import_block_sender: UnboundedSender<(
        sp_consensus::BlockOrigin,
        Vec<sc_consensus::IncomingBlock<B>>,
    )>,
    import_finalization_sender: UnboundedSender<(
        sp_consensus::BlockOrigin,
        Vec<sc_consensus::IncomingBlock<B>>,
    )>,
}

pub struct SplittedImportQueue {}

// TODO zamiast tego, mozna zrobic cos podobnego do splita, czyli jeden ImportQueue, pod mutexem, i oba o niego konkuruja

impl<B: Block> ImportQueue<B> for SplitImportQueue {
    fn import_blocks(
        &mut self,
        origin: sp_consensus::BlockOrigin,
        blocks: Vec<sc_consensus::IncomingBlock<B>>,
    ) {
        todo!()
    }

    fn import_justifications(
        &mut self,
        who: sc_consensus::import_queue::Origin,
        hash: <B as Block>::Hash,
        number: sp_api::NumberFor<B>,
        justifications: sp_runtime::Justifications,
    ) {
        todo!()
    }

    fn poll_actions(
        &mut self,
        cx: &mut futures::task::Context,
        link: &mut dyn sc_consensus::Link<B>,
    ) {
        todo!()
    }
}

fn split<B: Block, IQ: ImportQueue<B>>(iq: IQ) -> (SplitImportQueue, SplitImportQueue) {
    let (reciever, sender_one) = unbounded();
    let sender_two = sender_one.clone();
}

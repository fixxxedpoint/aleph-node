use std::sync::{Arc, Mutex};

use futures::channel::mpsc::{unbounded, UnboundedSender};
use sc_consensus::ImportQueue;
use sp_runtime::traits::Block;

#[derive(Clone)]
pub struct SplitImportQueue<B: Block, IQ: ImportQueue<B>> {
    import_queue: Arc<Mutex<IQ>>,
}

// TODO zamiast tego, mozna zrobic cos podobnego do splita, czyli jeden ImportQueue, pod mutexem, i oba o niego konkuruja

impl<B: Block, IQ: ImportQueue<B>> SplitImportQueue<B, IQ> {
    fn get_import_queue(&mut self) -> impl DerefMut<IQ> {
        self.import_queue
            .lock()
            .unwrap_or_else(|_| panic!("unable to use `import_queue`"))
    }
}

impl<B: Block> ImportQueue<B> for SplitImportQueue {
    fn import_blocks(
        &mut self,
        origin: sp_consensus::BlockOrigin,
        blocks: Vec<sc_consensus::IncomingBlock<B>>,
    ) {
        (&mut *self.get_import_queue()).import_blocks(origin, blocks);
    }

    fn import_justifications(
        &mut self,
        who: sc_consensus::import_queue::Origin,
        hash: <B as Block>::Hash,
        number: sp_api::NumberFor<B>,
        justifications: sp_runtime::Justifications,
    ) {
        (&mut *self.get_import_queue()).import_justifications(who, hash, number, justifications);
    }

    fn poll_actions(
        &mut self,
        cx: &mut futures::task::Context,
        link: &mut dyn sc_consensus::Link<B>,
    ) {
        (&mut *self.get_import_queue()).poll_actions(cx, link);
    }
}

fn split<B: Block, IQ: ImportQueue<B>>(iq: IQ) -> (SplitImportQueue, SplitImportQueue) {
    let wrapped_import_queue = Arc::new(Mutex::new(iq));
    let split_import_queue = SplitImportQueue::new(wrapped_import_queue);
    (split_import_queue.clone(), split_import_queue)
}

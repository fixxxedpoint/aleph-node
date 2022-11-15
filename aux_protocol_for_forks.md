# Problem statement:
Due to substrate's internal working, some forks are not synchronized, or are synchronized after very long time, which causes finalization to significantly slow down. Currently, we
request them manually, but because of substrate's mechanisms like `Reputation`, we are unable to finish such request. This interferes
with the DataAvailability mechanism, i.e. missing blocks for units. Here we propose an alternative mechanism for resolving this problem.

## Assumptions about substrate (needs to be verified)
(actually I would prefer to not make any assumptions about forks, but rather store them separately/externally to main chain
[without any additional checsk, just BlockImport'ed goes to such collection + some simple pruning])

1. an `imported` block from a non-hopeless fork will be kept indefinitely long *2. any non-hopeless fork will be synchronized at
some time in future (currently too slow?)
2. partially result of 1: a block that passes our DataAvailability mechanism can be imported, i.e. we have its branch
imported/it can be imported. *. any non-hopeless fork will be synchronized at some time in future (currently too slow?)

# Proposed solution
We create an auxiliary protocol just for handling `fork_requests`. Additionally, we remove previous ad-hoc changes made to
substrate, i.e. disabled `Reputation` mechanism. For clarity, we can divide this protocol/mechanism into two parts, i.e. fork
download and fork validation. <!-- Important part of this protocol is B --> Protocol messages - simple request/response
protocol:
1. DownloadForkRequest(Hash, Number)
2. DownloadForkResponse(Origin, [Blocks])

(actually since we know how to import some external Blocks, i.e. using the import_queue mechanism, the main problem here is to
device a fork-sync-protocol [part of new sync anyway]) Algorithm:
1. If ones discovers that he needs to download some fork, it sends `DownloadForkRequest(ForkHead::hash, ForkHeda::number)` to
   big enough subset of nodes.
2. A node that receives such request, if possible, answers with part of the forks branch (size limited, actually for simplicity
   it can just answer with a single block).
3. A node that requested fork download receives a fork-response and stores it in some auxiliary data structure to `substrate` -
   lets call it a fork-bag. Additionally, it can attempt to do some checks on such blocks immediately (not necessary, fork-bag
   needs to have limited size anyway for avoiding DOS attacks).
4. If requesting node discovers that that it already downloaded a branch (it should be able to fully import it) it removes it
   from its fork-bag and imports it using custom `Importer` (described below).

`Importer`: Assumptions/Discoveries: In order to `legally` import a block, it needs to be passed to substrate's `import_queue`.
We shouldn't call directly `BlockImport::import_block` trait (Client.rs), since it doesn't use for example
`BlockImport::check_block` and `Verifier::verify` (and probably some other checks).

It's a wrapped aura's `import_queue`, which we are explicitly using together with other substrate components. The only
modification to substrate `import_queue` is to allow it to accept blocks coming outside of network - it's passed there on
initialization.

NOTES: z wstepnej analizy wynika, ze wolanie import_queue z zewnatrz nie powinno niczego psuc.

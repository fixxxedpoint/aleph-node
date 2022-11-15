# Problem statement
Due to substrate's internal working, some forks are not getting synchronized, or are synchronized after very long time, which
causes finalization to significantly slow down. Currently, we request such forks manually, but because of substrate's internal
mechanisms like `Reputation`, we are unable to finish such requests within reasonable time-bounds (or at all). This interferes
with our `DataIO` mechanism, i.e. missing blocks for units, which cannot be accepted and processed. Here we propose an
alternative mechanism for resolving this problem.

## Assumptions about substrate^*

1. an `imported` block from a non-hopeless fork will be kept indefinitely long (at least until it stays non-hopelesss)
2. any non-hopeless fork will be synchronized with other nodes at some time (currently it is too slow, it causes stalls)
3. importing a non-hopeless `valid` fork should succeed (ImportQueue should let it through)

* from author: we can also consider making some more restrictive assumptions about forks in substrate and store such blocks
separately/externally to main chain and import them when ready (Still for some unknown reason it can reject such blocks? It
shouldn't since they are not hopeless.). We should probably keep such blocks until they become `hopeless`.

# Proposed solution

We can create an auxiliary protocol just for handling calls to `set_sync_fork_request` (calls from our own code). Additionally,
we can remove changes made in substrate, i.e. disabled `Reputation` mechanism. For clarity, we divide this problem into two
sub-problems, namely a custom request/response network protocol for downloading forks and second one for blocks validation and
block import mechanisms (download needs to be connected with some validation anyway - we at least want to keep/download blocks
that are correctly signed and that fulfills parent-child relation). We do not describe the network protocol here. We believe
such implementation could be also used for possible future sync-rewrite. We can create an in-house custom protocol using an
external `network` (similar to our substrate_network) or try using some of the substrate components. e.g. `ChainSync`. The main
technical issue here is block validation and how to to import them externally to substrate's network. Both validation and import
are required by alphbft's `DataIO` module - accepting only valid blocks and ensuring their availability (see assumptions about
forks). We considered two approaches, i.e.:
1. using `Verifier` and `BlockImport` directly
2. using `ImportQueue`

* Rejected solution: using directly just BlockImport - check_block -> import_block. This way we skip checks of `Verifier`.

Solution 1. is problematic, it requires us to use exactly same set of components (or their `shared` versions) as `aura` uses
internally and it is also more difficult to assure that we are actually checking all required conditions this way. It can also
interact in some non-obvious ways with the sync protocol. Solution 2. seems to be simpler and `safer`. ImportQueue is the main
entry point for the `block sync` protocol for write-interactions with local storage. Using it directly way we should be able to
execute same set of checks as normal `sync` procedure. We can create a `shared` version of ImportQueue and use it both in
network and our custom protocol. ImportQueue communicates with `sync` by means of its `poll_actions` method using the `Link`
trait - it receives notifications about imported blocks (e.g. ImportedUknown, BadBlock, etc.). After initial investigation,
`sync`'s `Link` implementation should co-op well if it receives notifications about blocks that it did not request or when it
receives more than one notification about same block (e.g. ImportUnknown followed by ImportKnown). We can also consider
performing a simple test: re-importing `some` (all/random/some) blocks manually using our custom shared ImportQueue (no custom
network protocol involved) and checking if everything is still green.

# Other options

Perhaps, in order to simplify `DataIO` we could enforce additional constraint about alephbft units that would allow us to
simplify block synchronization (blocks referenced by units). Initial proposal (which needs to be further investigated): for
every unit and its referenced block b, we can find a unit within its ancestors that references an ancestor block of block b, and
distance between b and such ancestor is bounded by some constant. This way if we were able to accept all of unit's ancestors
(other units) it should be fairly easy to synchronize a branch referenced by it (bounded by some constant number of blocks to
download). Obvious consequence of such approach is that finalization can't have `bigger` gaps between blocks (perhaps we should
consider slowing down block creation in such bad situations as well). Long gaps without finalization are `scary` anyway -
theoretically they can always be reverted.

Another alternative: if we see after some time that nobody references our top unit (i.e. nobody builds anything or our unit was
not referenced for at least one round) we start pro-actively sending its branch manually (blocks from last finalized) in
parent->child order - we just do what we are expect from substrate anyway. We can stop this process immediately after we realize
that its branch is hopeless.

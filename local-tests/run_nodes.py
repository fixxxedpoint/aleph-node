#!/bin/env python

# Short script demonstrating the basic usage of `chainrunner` package.
# Reproduces (more or less) the behavior of `run_nodes.sh`.
# For running local experiments it's much more convenient to manage the chain
# using an interactive environment (Python console, Jupyter notebook etc.)

from chainrunner import Chain, Seq, generate_keys, check_finalized

nodes = 4
workdir = '.'
aleph_node_binary = '../target/release/aleph-node'
chain_bootstrapper_binary = '../target/release/chain-bootstrapper'
port = 30334
rpc_port = 9944

phrases = ['//Alice', '//Bob', '//Charlie', '//Dave', '//Ezekiel', '//Fanny', '//George', '//Hugo']
keys_dict = generate_keys(aleph_node_binary, phrases)
keys = list(keys_dict.values())
nodes = min(nodes, len(phrases))

chain = Chain(workdir)

print(f'Bootstrapping chain for {nodes} nodes')
chain.bootstrap(aleph_node_binary,
                chain_bootstrapper_binary,
                keys[:nodes],
                chain_type='local')
chain.set_flags('validator',
                'unsafe-ws-external',
                'unsafe-rpc-external',
                'no-mdns',
                port=Seq(port),
                rpc_port=Seq(rpc_port),
                unit_creation_delay=500,
                execution='Native',
                rpc_cors='all',
                rpc_methods='Unsafe')
addresses = [n.address() for n in chain]
chain.set_flags(bootnodes=addresses[0], public_addr=addresses)

print('Starting the chain')
chain.start('node')

print('Waiting for finalization')
chain.wait_for_finalization(0)

check_finalized(chain)

print('Exiting script, leaving nodes running in the background')

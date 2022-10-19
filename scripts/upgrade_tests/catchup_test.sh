#!/bin/bash

set -euo pipefail

OVERRIDE_DOCKER_COMPOSE=${OVERRIDE_DOCKER_COMPOSE:-""}
export OVERRIDE_DOCKER_COMPOSE
INIT_BLOCK=${INIT_BLOCK:-3}
UPGRADE_ROUND=${UPGRADE_ROUND:-10}
UPGRADE_VERSION=${UPGRADE_VERSION:-2}
VALIDATOR=${VALIDATOR:-"Node1"}
RPC_HOST=${RPC_HOST:-127.0.0.1}
RPC_PORT=${RPC_PORT:-9933}
NODES=${NODES:-""}

function initialize {
    local init_block=$1
    local node=$2
    local address=$3
    while [[ $(get_best_finalized) -le $init_block ]]; do
        sleep 1
    done
}

function get_best_finalized {
    local validator=$1
    local rpc_address=$2
    local rpc_port=$3

    echo $( VALIDATOR=$validator RPC_HOST=$rpc_address RPC_PORT=$rpc_port ./.github/scripts/check_finalization.sh | sed 's/Last finalized block number: //' )

    # local finalized=""
    # while  [[ -z "$finalized" ]]; do
    #     finalized=$( VALIDATOR ./.github/scripts/check_finalization.sh | sed 's/Last finalized block number: //' )
    # done
    # echo $finalized
}

function set_upgrade_round {
    local round=$1
    local version=$2

    docker run --network container:$VALIDATOR appropriate/curl:latest \
             -H "Content-Type: application/json" \
             -d '{"id":1, "jsonrpc":"2.0", "method": "aleph_setUpgrade", "params": ['$round', '$version']}' http://$RPC_HOST:$RPC_PORT | jq '.result'
}

function disable_nodes {
    local nodes=$1
    for node in $nodes; do
        docker network disconnect main ${node}
    done
}

function enable_nodes {
    local nodes=$1
    for node in $nodes; do
        docker network connect main ${node}
    done
}

function wait_for_round {
    local round=$1

    initialize $1
}

function get_last_block {
}

function check_finalization {
    local block_to_check=$1
    local nodes=$2
    local addresses=$3
    local ports=$4

    for i in "${!nodes[@]}"; do
        local node=node[i]
        local address=addresses[i]

        initialize ${block_to_check} ${node} ${address}
    done
}

./.github/scripts/run_consensus.sh

initialize ${INIT_BLOCK}

disable_nodes ${NODES}

set_upgrade_round ${UPGRADE_ROUND} ${UPGRADE_VERSION}

wait_for_round ${ROUND}

enable_nodes ${NODES}

last_block=$(get_last_block ${VALIDATOR} ${VALIDATOR_ADDRESS} ${VALIDATOR_PORT})

check_finalization $($last_block+1) ${ALL_NODES} ${ADDRESSES}

exit $?

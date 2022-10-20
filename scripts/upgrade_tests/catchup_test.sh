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
UPGRADE_BEFORE_DISABLE=${UPGRADE_BEFORE_DISABLE:-false}
SEED=${SEED:-"//0"}
ALL_NODES_RPC=${ALL_NODES_RPC:-(TODO)}

function initialize {
    local init_block=$1
    local node=$2
    local port=$3

    while [[ $(get_best_finalized ${node} ${port}) -le $init_block ]]; do
        sleep 1
    done
}

function get_best_finalized {
    local validator=$1
    local rpc_port=$2

    VALIDATOR=${validator} RPC_HOST="localhost" RPC_PORT=${rpc_port} ./.github/scripts/check_finalization.sh | sed 's/Last finalized block number: //'
}

function set_upgrade_round {
    local round=$1
    local version=$2
    local validator=$3
    local seed=$4

    docker run --network container:$validator cliain:latest --node 127.0.0.1:9933 --seed "${seed}" version_upgrade_schedule --version ${version} --session ${round}
}

function disconnect_nodes {
    local nodes=$1
    for node in $nodes; do
        docker network disconnect main-network ${node}
    done
}

function connect_nodes {
    local nodes=$1
    for node in $nodes; do
        docker network connect main-network ${node}
    done
}

function wait_for_round {
    local round=$1
    local validator=$2
    local rpc_port=$3

    initialize $round
    local last_block=""
    while [[ -z "$last_block" ]]; do
        last_block=$(docker run --network container:$validator appropriate/curl:latest \
               -H "Content-Type: application/json" \
               -d '{"id":1, "jsonrpc":"2.0", "method": "chain_getBlockHash", "params": '$round'}' http://127.0.0.1:${rpc_port} | jq '.result')
    done
}

function get_last_block {
    local validator=$1
    local rpc_port=$2

    last_block_hash=$(docker run --network container:$validator appropriate/curl:latest \
              -H "Content-Type: application/json" \
              -d '{"id":1, "jsonrpc":"2.0", "method": "chain_getBlockHash"}' http://127.0.0.1:$rpc_port | jq '.result')
    docker run --network container:$validator appropriate/curl:latest \
           -H "Content-Type: application/json" \
           -d '{"id":1, "jsonrpc":"2.0", "method": "chain_getBlockHash"}' http://127.0.0.1:$rpc_port | jq '.result'
}

function check_finalization {
    local block_to_check=$1
    local nodes=$2
    local addresses=$3
    local ports=$4

    for i in "${!nodes[@]}"; do
        local node=nodes[i]
        local address=addresses[i]
        local port=ports[i]

        initialize ${block_to_check} ${address} ${port}
    done
}

OVERRIDE_DOCKER_COMPOSE=./docker/docker-compose.bridged.yml ./.github/scripts/run_consensus.sh

initialize ${INIT_BLOCK} "Node4" 9933

if [[ $UPGRADE_BEFORE_DISABLE = true ]]; then
    set_upgrade_round ${UPGRADE_ROUND} ${UPGRADE_VERSION} ${VALIDATOR} ${SEED}
fi

disconnect_nodes ${NODES}

if [[ $UPGRADE_BEFORE_DISABLE = false ]]; then
    set_upgrade_round ${UPGRADE_ROUND} ${UPGRADE_VERSION} ${VALIDATOR} ${SEED}
fi

wait_for_round ${ROUND} ${VALIDATOR} ${RPC_PORT}

connect_nodes ${NODES}

last_block=$(get_last_block ${VALIDATOR} ${RPC_PORT})

check_finalization $($last_block+1) ${ALL_NODES_RPC}

exit $?

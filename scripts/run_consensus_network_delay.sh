#!/bin/env bash

set -euo pipefail

source ./scripts/common.sh

function usage(){
    cat << EOF
Usage:
  $0
     --network-delay DELAYms
        simulated network delay in ms; default=500ms
    --no-build-image
        skip docker image build
    --nodes "Node0:Node1"
        list of nodes (Node0..Node4) for this script to handle; default="Node0:Node1:Node2:Node3:Node4"
    --node-ports "9934:9935"
        list of rpc ports for --nodes; default="9933:9934:9935:9936:9937"
    --check-block number
        check finalization for a given block number, 0 means no-check; default=42
EOF
    exit 0
}

function build_test_image() {
    docker build -t aleph-node:network_tests -f docker/Dockerfile.network_tests .
}

function set_network_delay() {
    local node=$1
    local delay=$2

    log "setting network delay for node $node"
    docker exec $node tc qdisc add dev eth1 root netem delay ${delay}ms
}

while [[ $# -gt 0 ]]; do
    case $1 in
        --network-delay)
            NETWORK_DELAY="$2"
            shift;shift
            ;;
        --no-build-image)
            BUILD_IMAGE=false
            shift
            ;;
        --nodes)
            NODES="$2"
            shift;shift
            ;;
        --node-ports)
            NODES_PORTS="$2"
            shift;shift
            ;;
        --check-block)
            CHECK_BLOCK_FINALIZATION="$2"
            shift;shift
            ;;
        --help)
            usage
            shift
            ;;
        *)
            error "Unrecognized argument $1!"
            ;;
    esac
done

NETWORK_DELAY=${NETWORK_DELAY:-500}
BUILD_IMAGE=${BUILD_IMAGE:-true}
NODES=${NODES:-"Node0:Node1:Node2:Node3:Node4"}
NODES_PORTS=${NODES_PORTS:-"9933:9934:9935:9936:9937"}
CHECK_BLOCK_FINALIZATION=${CHECK_BLOCK_FINALIZATION:-42}

into_array $NODES
NODES=(${result[@]})

into_array $NODES_PORTS
NODES_PORTS=(${result[@]})

if [[ "$BUILD_IMAGE" = true ]]; then
    log "building custom docker image for network tests"
    build_test_image
fi

log "starting network"
OVERRIDE_DOCKER_COMPOSE=./docker/docker-compose.network_tests.yml DOCKER_COMPOSE=./docker/docker-compose.bridged.yml ./.github/scripts/run_consensus.sh 1>&2
log "network started"

log "setting network delay"
for node in ${NODES[@]}; do
    set_network_delay $node $NETWORK_DELAY
done

if [[ $CHECK_BLOCK_FINALIZATION -gt 0 ]]; then
    log "checking finalization"
    check_finalization $CHECK_BLOCK_FINALIZATION NODES NODES_PORTS
    log "finalization checked"
fi

log "done"

exit 0

#!/usr/bin/env bash
set -euo pipefail

SPAWN_SHELL=${SPAWN_SHELL:-false}
NIX_FILE=${NIX_FILE:-"default.nix"}
DYNAMIC_LINKER_PATH=${DYNAMIC_LINKER_PATH:-"/lib64/ld-linux-x86-64.so.2"}
CRATES=${CRATES:-'{ "aleph-node" = []; }'}
SINGLE_STEP=${SINGLE_STEP:-'false'}
RUSTFLAGS=${RUSTFLAGS:-'"-C target-cpu=generic"'}
if [ -z ${PATH_TO_FIX+x} ]; then
    PATH_TO_FIX="result/bin/aleph-node"
fi

function usage(){
    echo "Usage:
      ./nix-build.sh [-s - spawn nix-shell]"
}

while getopts "s" flag
do
    case "${flag}" in
        s) SPAWN_SHELL=true;;
        *)
            usage
            exit
            ;;
    esac
done

if [ $SPAWN_SHELL = true ]
then
    nix-shell --pure $NIX_FILE
else
    ARGS=(--arg crates "${CRATES}" --arg singleStep "${SINGLE_STEP}" --arg rustflags "${RUSTFLAGS}")

    # we need to download all dependencies
    echo fetching depedencies...
    CARGO_HOME="$(realpath ~/.cargo)"

    set +e
    nix-shell --show-trace --pure --run "CARGO_HOME=$CARGO_HOME cargo fetch --locked --offline"
    EXITCODE=$?
    set -e

    if [ ! $EXITCODE -eq 0 ]; then
        echo need to access network to download rust dependencies
        nix-shell --show-trace --pure --run "CARGO_HOME=$CARGO_HOME cargo fetch --locked"
    fi

    echo building...
    nix-build --show-trace --max-jobs auto --option sandbox true --arg cargoHomePath "$CARGO_HOME" $NIX_FILE "${ARGS[@]}"
    echo build finished

    echo copying results...
    mv result result.orig
    cp -Lr result.orig result
    rm result.orig
    chmod -R 777 result
    echo results copied

    # we need to change the dynamic linker
    # otherwise our binary references one that is specific for nix
    # we need it for aleph-node to be run outside nix-shell
    if [ ! -z "$PATH_TO_FIX" && -f $PATH_TO_FIX ]; then
        echo patching...
        chmod +w $PATH_TO_FIX
        patchelf --set-interpreter $DYNAMIC_LINKER_PATH $PATH_TO_FIX
    fi
    echo nix-build.sh finished
fi

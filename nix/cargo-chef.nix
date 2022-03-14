{ pkgs ? import <nixpkgs> {}
, stdenv ? pkgs.stdenv
, buildRustCrate ? pkgs.buildRustCrate
, crateDir
}:
let
  cargo-fetcher = (pkgs.runCommandCC "cargo-chef" { nativeBuildInputs = [ pkgs.cargo pkgs.rustc pkgs.cacert ]; } ''
    export CARGO_HOME=$out/cargo
    mkdir -p $CARGO_HOME
    mkdir -p $out/bin
    cargo install cargo-fetcher --locked --version 0.12.1 --features=fs
    cp $CARGO_HOME/bin/cargo-fetcher $out/bin/
  '').overrideAttrs (_: { inherit stdenv; });

  cachedDependencies = cargoLock: pkgs.runCommand "cargo-chef cook" { nativeBuildInputs = [ cargo-fetcher pkgs.cargo pkgs.rustc pkgs.cacert ]; } ''
    TMP=$out/tmp
    export CARGO_HOME=$TMP/.cargo-home
    mkdir -p $CARGO_HOME
    echo ${cargoLock} >$TMP/Cargo.lock

    cargo-fetcher --include-index --lock-file $TMP/Cargo.lock --url file:$CARGO_HOME sync
    mv $CARGO_HOME/* $out/
    rm -rf $TMP
  '';

in
cachedDependencies (builtins.readFile "${buildRecipe}")

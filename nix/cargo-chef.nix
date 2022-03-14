{ pkgs ? import <nixpkgs> {}
, stdenv ? pkgs.stdenv
, buildRustCrate ? pkgs.buildRustCrate
, crateDir
}:
let
  # cargo-chef =
  #   let
  #     cargo-chef = buildRustCrate {
  #       crateName = "cargo-chef";
  #       version = "0.1.34";
  #       sha256 = "XVCCAQeh9bz3Pp0pLgKXizvF8EtagksHjwNz0U//xQs=";
  #     };
  #   in
  #   builtins.symlinkJoin { name = "cargo-chef"; paths = [ cargo-chef ]; };

  # cargo-chef = buildRustCrate {
  #   crateName = "cargo-chef";
  #   version = "0.1.34";
  #   sha256 = "XVCCAQeh9bz3Pp0pLgKXizvF8EtagksHjwNz0U//xQs=";
  # };

  # cargo-chef = pkgs.runCommand "cargo-chef" { nativeBuildInputs = [ pkgs.cargo pkgs.rustc pkgs.cacert ]; } ''
  #   export CARGO_HOME=$out/cargo
  #   mkdir -p $CARGO_HOME
  #   mkdir -p $out/bin
  #   cargo install cargo-chef --locked
  #   cp $CARGO_HOME/bin/cargo-chef $out/bin/
  # '';

  cargo-chef = (pkgs.callPackage pkgs.runCommandCC { inherit stdenv; }) "cargo-chef" { nativeBuildInputs = [ pkgs.cargo pkgs.rustc pkgs.cacert ]; } ''
    export CARGO_HOME=$out/cargo
    mkdir -p $CARGO_HOME
    mkdir -p $out/bin
    cargo install cargo-chef --locked
    cp $CARGO_HOME/bin/cargo-chef $out/bin/
  '';

  buildRecipe = pkgs.runCommand "cargo-chef prepare" { nativeBuildInputs = [ cargo-chef pkgs.cargo pkgs.rustc pkgs.cacert ]; } ''
    cd ${crateDir}
    cargo-chef prepare --recipe-path $out
  '';

  cachedDependencies = recipeJson: pkgs.runCommand "cargo-chef cook" { nativeBuildInputs = [ cargo-chef pkgs.cargo pkgs.rustc pkgs.cacert ]; } ''
    TMP=$out/tmp
    export CARGO_HOME=$TMP/.cargo-home
    mkdir -p $CARGO_HOME
    echo ${recipeJson} >$TMP/recipe.json

    cargo-chef cook --recipe-path $TMP/recipe.json
    mv $CARGO_HOME/* $out/
    rm -rf $TMP
  '';

in
cachedDependencies (builtins.readFile "${buildRecipe}")

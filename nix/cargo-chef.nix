{ pkgs ? import <nixpkgs> {}
, stdenv ? pkgs.stdenv
, buildRustCrate ? pkgs.buildRustCrate
, crateDir
}:
let
  cargo-chef = (pkgs.runCommandCC "cargo-chef" { nativeBuildInputs = [ pkgs.cargo pkgs.rustc pkgs.cacert ]; } ''
    export CARGO_HOME=$out/cargo
    mkdir -p $CARGO_HOME
    mkdir -p $out/bin
    cargo install cargo-chef --locked
    cp $CARGO_HOME/bin/cargo-chef $out/bin/
  '').overrideAttrs (_: { inherit stdenv; });

  buildRecipe = pkgs.runCommand "cargo-chef prepare" { nativeBuildInputs = [ cargo-chef pkgs.cargo pkgs.rustc pkgs.cacert ]; } ''
    cd ${crateDir}
    cargo-chef chef prepare --recipe-path $out
  '';

  storedRecipe = builtins.toFile "recipe.json" (builtins.readFile buildRecipe);

  cachedDependencies = recipeJson: (pkgs.runCommandCC "cargo-chef cook" { nativeBuildInputs = [ cargo-chef pkgs.cargo pkgs.rustc pkgs.cacert ]; } ''
    TMP=$out/tmp
    export CARGO_HOME=$TMP/.cargo-home
    mkdir -p $CARGO_HOME

    yes yes | cargo-chef chef cook --recipe-path ${recipeJson} || true
    mv $CARGO_HOME/* $out/
    rm -rf $TMP
  '').overrideAttrs (_: { inherit stdenv; });
in
cachedDependencies storedRecipe

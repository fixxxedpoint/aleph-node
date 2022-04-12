{
  inputs = {
    # cargo2nix.url = "path:../../";
    # Use the github URL for real packages
    # cargo2nix.url = "github:cargo2nix/cargo2nix/master";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay.url = "github:oxalica/rust-overlay";
    rust-overlay.inputs.nixpkgs.follows = "nixpkgs";
    rust-overlay.inputs.flake-utils.follows = "flake-utils";
    nixpkgs.url = "github:nixos/nixpkgs?ref=release-21.05";
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay, ... }:

    # Build the output set for each default system and map system sets into
    # attributes, resulting in paths such as:
    # nix build .#packages.x86_64-linux.<name>
    flake-utils.lib.eachDefaultSystem (system:

      # let-in expressions, very similar to Rust's let bindings.  These names
      # are used to express the output but not themselves paths in the output.
      let

        cargo2nix = ( import ./nix/cargo2nix.nix ).cargo2nixSrc;
        # create nixpkgs that contains rustBuilder from cargo2nix overlay
        pkgs = import nixpkgs {
          inherit system;
          overlays = [(import "${cargo2nix}/overlay")
                      rust-overlay.overlay];
        };

        rustChannel = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain;
	rustToolchain' = with pkgs; (rust-bin.${rustChannel}.minimal).override {
	  targets = [ "x86_64-unknown-linux-gnu" "wasm32-unknown-unknown" ];
	};
	# rustToolchain' = with pkgs; (rust-bin.nightly.${rustChannel}.minimal);
        rustToolchain = builtins.trace rustToolchain' rustToolchain';

        # create the workspace & dependencies package set
        rustPkgs = (pkgs.rustBuilder.makePackageSet {
          # rustChannel = "${rustToolchain}/";
          # rustChannel = "1.56.1";
          # rustChannel = "nightly-2021-10-24";
          rustChannel = "/nix/store/a0mcw9giagr7dbz7gr9g0cmby7094hzd-rust-1.58.0-nightly-2021-10-23-91b931926/";
          packageFun = import ./Cargo.nix;
          target = "x86_64-unknown-linux-gnu";
          # packageOverrides = pkgs: pkgs.rustBuilder.overrides.all; # Implied, if not specified
          packageOverrides = pkgs: pkgs.rustBuilder.overrides.all ++ [
            # parentheses disambiguate each makeOverride call as a single list element
            (pkgs.rustBuilder.rustLib.makeOverride {
              name = "names";
              overrideAttrs = drv: {
                features = ["application"];
              };
            })
          ];
        });

      in rec {
        # this is the output (recursive) set (expressed for each system)

        # the packages in `nix build .#packages.<system>.<name>`
        packages = {
          # nix build .#aleph-node
          # nix build .#packages.x86_64-linux.aleph-node
          aleph-node = (rustPkgs.workspace.aleph-node {}).bin;
        };

        # nix build
        defaultPackage = packages.aleph-node;
      }
    );
}

#
# Some tools that might be useful in builds.
#
# Part of the "public" API of crate2nix in the sense that we will try to
# avoid breaking the API and/or mention breakages in the CHANGELOG.
#

{ pkgs ? import ./nix/nixpkgs.nix { config = { }; }
, lib ? pkgs.lib
, stdenv ? pkgs.stdenv
, strictDeprecation ? true
, crate2nix ? pkgs.crate2nix
}:
rec {

  /* Returns the whole top-level function generated by crate2nix (`Cargo.nix`)
    which is typically called with `pkgs.callPackage`.

    name: will be part of the derivation name
    src: the source that is needed to build the crate, usually the
    crate/workspace root directory
    cargoToml: Path to the Cargo.toml file relative to src, "Cargo.toml" by
    default.
  */
  generatedCargoNix =
    { name
    , src
    , cargoToml ? "Cargo.toml"
    , additionalCargoNixArgs ? [ ]
    }:
    stdenv.mkDerivation {
      name = "${name}-crate2nix";

      buildInputs = [ pkgs.git pkgs.rustc pkgs.cargo pkgs.jq crate2nix pkgs.cacert ];
      preferLocalBuild = true;

      inherit src;
      phases = [ "unpackPhase" "buildPhase" ];

      buildPhase = ''
        set -e

        mkdir -p "$out/cargo"

        export CARGO="${pkgs.cargo}/bin/cargo"
        export CARGO_HOME="$out/cargo"
        export HOME="$out"
        export CARGO_NET_GIT_FETCH_WITH_CLI=true

        crate_hashes="$out/crate-hashes.json"

        create2nix_options=" -f ./${cargoToml}"

        set -x

        {
          crate2nix generate \
            $create2nix_options \
            -o "Cargo-generated.nix" \
            -h "$crate_hashes" \
            ${lib.escapeShellArgs additionalCargoNixArgs}
        }
        { set +x; } 2>/dev/null

        cp -r . $out/crate

        echo "import ./crate/Cargo-generated.nix" > $out/default.nix
      '';

    };
}

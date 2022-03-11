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

      buildInputs = [ pkgs.cargo pkgs.jq crate2nix ];
      preferLocalBuild = true;

      inherit src;
      phases = [ "unpackPhase" "buildPhase" ];

      buildPhase = ''
        set -e

        mkdir -p "$out/cargo"

        export CARGO_HOME="$out/cargo"
        export HOME="$out"

        crate_hashes="$out/crate-hashes.json"

        create2nix_options=" -f ./${cargoToml}"

        if test -r "${src}/crate2nix-sources" ; then
          ln -s "${src}/crate2nix-sources" "$out/crate2nix-sources"
        fi

        set -x

        crate2nix generate \
          $create2nix_options \
          -o "Cargo-generated.nix" \
          -h "$crate_hashes" \
          ${lib.escapeShellArgs additionalCargoNixArgs} || {
          { set +x; } 2>/dev/null
          echo "crate2nix failed." >&2
          echo "== cargo/config (BEGIN)" >&2
          sed 's/^/    /' $out/cargo/config >&2
          echo ""
          echo "== cargo/config (END)" >&2
            echo ""
            echo "== crate-hashes.json (BEGIN)" >&2
          if [ -r $crate_hashes ]; then
            sed 's/^/    /' $crate_hashes >&2
            echo ""
          else
            echo "$crate_hashes missing"
          fi
          echo "== crate-hashes.json (END)" >&2
          echo ""
          echo "== ls -la (BEGIN)" >&2
          ls -la
          echo "== ls -la (END)" >&2
          exit 3
        }
        { set +x; } 2>/dev/null

        cp -r . $out/crate

        echo "import ./crate/Cargo-generated.nix" > $out/default.nix
      '';

    };
}

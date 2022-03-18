/* Declares two helper functions, i.e. generatedCargoNix and vendoredCargoLock. Former, generates
   a nix file in ad-hoc manner based on projects Cargo.lock and Cargo.toml files.
   Later, provides all git dependencies for other derivations (cargo is unable to access internet
   while in sanbox mode).
*/
{ versions ? import ./versions.nix
, pkgs ? versions.nixpkgs
, lib ? pkgs.lib
, stdenv ? pkgs.stdenv
, strictDeprecation ? true
, crate2nix ? pkgs.crate2nix
, importCargoLock ? versions.importCargoLock
}:
let
  outputHashes = crateDir:
    let
      lockFile = crateDir + "/Cargo.lock";

      locked = (builtins.fromTOML (builtins.readFile lockFile)).package or [];

      toPackageId = { name, version, source, ... }:
              "${name} ${version} (${source})";

      toPackageIdForImportCargoLock = { name, version, source, ... }:
              "${name}-${version}";

      parseGitSource = source:
        let
          withoutGitPlus = lib.removePrefix "git+" source;
          splitHash = lib.splitString "#" withoutGitPlus;
          splitQuestion = lib.concatMap (lib.splitString "?") splitHash;
        in
        {
          url = builtins.head splitQuestion;
          rev = lib.last splitQuestion;
        };

      mkGitHash = { source, name, ... }@attrs:
        let
          parsed = parseGitSource source;
          src = builtins.fetchGit {
            submodules = true;
            inherit (parsed) url rev;
            allRefs = true;
          };
          hash = pkgs.runCommand "hash-of-${name}" { nativeBuildInputs = [ pkgs.nix ]; } ''
            echo -n "$(nix-hash --type sha256 ${src})" > $out
          '';
        in
        builtins.readFile hash;

      isGitSource = { source ? null, ... }:
        lib.hasPrefix "git+" source;

      packages =
        let
          packagesWithoutLocal = builtins.filter (p: p ? source) locked;
          packageById = package: { name = toPackageId package; value = package; };
          # it removes duplicates (takes first occurrence)
          packagesById = builtins.listToAttrs (builtins.map packageById packagesWithoutLocal);
        in
        builtins.attrValues packagesById;

      gitPackages = builtins.filter isGitSource packages;

      packageToNamedHash = toPackageIdFun: package: { name = toPackageIdFun package; value = mkGitHash package; };

      extraHashes = builtins.listToAttrs (map (packageToNamedHash toPackageId) gitPackages);

      extraHashesForImportCargoLock = builtins.listToAttrs (map (packageToNamedHash toPackageIdForImportCargoLock) gitPackages);
  in
  { inherit extraHashes extraHashesForImportCargoLock; };
in
rec {

  /* Returns the whole top-level function generated by crate2nix (`Cargo.nix`)
     which is typically called with `pkgs.callPackage`. Generated function describes
     all derivations needed to build our project.

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
    let
      crateDir = dirOf (src + "/${cargoToml}");
      cargoLock = builtins.readFile (src + "/Cargo.lock");

      hashes = outputHashes crateDir;
      extraHashesForImportCargoLock = hashes.extraHashesForImportCargoLock;
      extraHashes = hashes.extraHashes;
      # this downloads all of our build dependencies (rust) and stores them locally in /nix/store
      vendoredCargo = importCargoLock { lockFileContents = cargoLock; outputHashes = extraHashesForImportCargoLock; };
    in
    stdenv.mkDerivation {
      name = "${name}-crate2nix";

      buildInputs = [ pkgs.cacert pkgs.git pkgs.rustc pkgs.cargo pkgs.jq crate2nix ];
      preferLocalBuild = true;

      inherit src;
      phases = [ "unpackPhase" "buildPhase" ];

      buildPhase = ''
        set -e

        # we need to propagate CARGO_HOME with all of our git dependencies
        CARGO_HOME_BASE="$out/.cargo-home"
        export CARGO_HOME="$out/.cargo-home/.cargo"
        mkdir -p $CARGO_HOME_BASE
        ln -s ${vendoredCargo}/.cargo $CARGO_HOME
        ln -s ${vendoredCargo} $CARGO_HOME/../cargo-vendor-dir
        ln -s ${vendoredCargo}/Cargo.lock $CARGO_HOME/../Cargo.lock
        export HOME="$out"

        # we calculate hashes of all of the dependencies
        crate_hashes="$out/crate-hashes.json"
        printf '${builtins.toJSON extraHashes}' > "$crate_hashes"

        crate2nix_options=""
        if [ -r ./${cargoToml} ]; then
          create2nix_options+=" -f ./${cargoToml}"
        fi

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

  # allows to propagate downloaded crates to other derivations
  vendoredCargoLock = src: cargoToml:
    let
      crateDir = dirOf (src + "/${cargoToml}");
      cargoLock = builtins.readFile (src + "/Cargo.lock");
      extraHashesForImportCargoLock = (outputHashes crateDir).extraHashesForImportCargoLock;
    in
    importCargoLock { lockFileContents = cargoLock; outputHashes = extraHashesForImportCargoLock; };
}

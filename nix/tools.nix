{ pkgs ? import ./nix/nixpkgs.nix {}
, lib ? pkgs.lib
, stdenv ? pkgs.stdenv
, strictDeprecation ? true
, crate2nix ? pkgs.crate2nix
}:
let
  outputHashes = crateDir:
    let
      toPackageId = { name, version, source, ... }:
              "${name} ${version} (${source})";

      toPackageIdForImportCargoLock = { name, version, source, ... }:
              "${name}-${version}";

      lockFiles =
        let
          fromCrateDir =
            if builtins.pathExists (crateDir + "/Cargo.lock")
            then [ (crateDir + "/Cargo.lock") ]
            else [ ];
          fromSources =
            if builtins.pathExists (crateDir + "/crate2nix-sources")
            then
              let
                subdirsTypes = builtins.readDir (crateDir + "/crate2nix-sources");
                subdirs = builtins.attrNames subdirsTypes;
                toLockFile = subdir: (crateDir + "/crate2nix-sources/${subdir}/Cargo.lock");
              in
              builtins.map toLockFile subdirs
            else [ ];
        in
        fromCrateDir ++ fromSources;

      locked =
        let
          parseFile = cargoLock: builtins.fromTOML (builtins.readFile cargoLock);
          allParsedFiles = builtins.map parseFile lockFiles;
          merge = merged: lock:
            {
              package = merged.package ++ lock.package or [ ];
              metadata = merged.metadata // lock.metadata or { };
            };
        in
        lib.foldl merge { package = [ ]; metadata = { }; } allParsedFiles;

      hashesFiles =
        builtins.map
          (cargoLock: "${dirOf cargoLock}/crate-hashes.json")
          lockFiles;

      hashes =
        let
          parseFile = hashesFile:
            if builtins.pathExists hashesFile
            then builtins.fromJSON (builtins.readFile hashesFile)
            else { };
          parsedFiles = builtins.map parseFile hashesFiles;
        in
        lib.foldl (a: b: a // b) { } parsedFiles;

      unhashedGitDeps = builtins.filter (p: ! hashes ? ${toPackageId p}) packagesByType.git or [ ];

      # Extracts URL and rev from a git source URL.
      #
      # Crude, should be more robust :(
      parseGitSource = source:
        assert builtins.isString source;
        let
          withoutGitPlus = lib.removePrefix "git+" source;
          splitHash = lib.splitString "#" withoutGitPlus;
          splitQuestion = lib.concatMap (lib.splitString "?") splitHash;
        in
        {
          url = builtins.head splitQuestion;
          rev = lib.last splitQuestion;
        };

      mkGitHash = { toPackageIdFun ? toPackageId }: { source, ... }@attrs:
        let
          parsed = parseGitSource source;
          src = builtins.fetchGit {
            submodules = true;
            inherit (parsed) url rev;
            allRefs = true;
          };
          hash = pkgs.runCommand "hash-of-${attrs.name}" { nativeBuildInputs = [ pkgs.nix ]; } ''
            echo -n "$(nix-hash --type sha256 ${src})" > $out
          '';
        in
        {
          name = toPackageIdFun attrs;
          value = builtins.readFile hash;
        };

      extraHashes = builtins.listToAttrs (map (mkGitHash { toPackageIdFun = toPackageId; }) unhashedGitDeps);
      extraHashesForImportCargoLock = builtins.listToAttrs (map (mkGitHash { toPackageIdFun = toPackageIdForImportCargoLock; }) unhashedGitDeps);

      sourceType = { source ? null, ... } @ package:
        assert source == null || builtins.isString source;

        if source == null then
          null
        else if source == "registry+https://github.com/rust-lang/crates.io-index" then
          "crates-io"
        else if lib.hasPrefix "git+" source then
          "git"
        else
          builtins.throw "unknown source type: ${source}";

      packages =
        let
          packagesWithDuplicates = assert builtins.isList locked.package; locked.package;
          packagesWithoutLocal = builtins.filter (p: p ? source) packagesWithDuplicates;
          packageById = package: { name = toPackageId package; value = package; };
          packagesById = builtins.listToAttrs (builtins.map packageById packagesWithoutLocal);
        in
        builtins.attrValues packagesById;
      packagesWithType = builtins.filter (pkg: (sourceType pkg) != null) packages;
      packagesByType = lib.groupBy sourceType packagesWithType;
  in
  { inherit extraHashes extraHashesForImportCargoLock; };
in
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
    let
      crateDir = dirOf (src + "/${cargoToml}");
      cargoLock = builtins.readFile (src + "/Cargo.lock");
      importCargoLock = (import (builtins.fetchTarball {
        url = "https://github.com/NixOS/nixpkgs/archive/be872a7453a176df625c12190b8a6c10f6b21647.tar.gz";
        sha256 = "1hnwh2w5rhxgbp6c8illcrzh85ky81pyqx9309bkgpivyzjf2nba";
      }) {}).rustPlatform.importCargoLock;

      hashes = outputHashes crateDir;
      extraHashesForImportCargoLock = hashes.extraHashesForImportCargoLock;
      extraHashes = hashes.extraHashes;
      vendoredCargoLock = importCargoLock { lockFileContents = cargoLock; outputHashes = extraHashesForImportCargoLock; };
      vendoredCargoConfig = vendoredCargoLock + "/.cargo/config";
    in
    stdenv.mkDerivation {
      name = "${name}-crate2nix";

      buildInputs = [ pkgs.cacert pkgs.git pkgs.rustc pkgs.cargo pkgs.jq crate2nix ];
      preferLocalBuild = true;

      inherit src;
      phases = [ "unpackPhase" "buildPhase" ];

      buildPhase = ''
        set -e

        mkdir -p "$out/cargo"
        cp -r ${vendoredCargoConfig} $out/cargo/config
        ln -s ${vendoredCargoLock} $out/cargo-vendor-dir

        export CARGO_HOME="$out/cargo"
        export HOME="$out"

        crate_hashes="$out/crate-hashes.json"
        if test -r "./crate-hashes.json" ; then
          printf "$(jq -s '.[0] * ${builtins.toJSON extraHashes}' "./crate-hashes.json")" > "$crate_hashes"
          chmod +w "$crate_hashes"
        else
          printf '${builtins.toJSON extraHashes}' > "$crate_hashes"
        fi

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
}

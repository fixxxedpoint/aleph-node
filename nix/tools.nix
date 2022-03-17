/* Declares two helper functions, i.e. generatedCargoNix and vendoredCargoLock. Former, generates
   a nix file in ad-hoc manner based on projects Cargo.lock and Cargo.toml files.
   Later, provides all cargo dependencies for other derivations (cargo is unable to access internet
   while in sanbox mode).
*/
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
          (cargoLock: (dirOf cargoLock) + "/crate-hashes.json")
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

      mkGitHash = toPackageIdFun: { source, ... }@attrs:
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

      extraHashes = builtins.listToAttrs (map (mkGitHash toPackageId) unhashedGitDeps);
      extraHashesForImportCargoLock = builtins.listToAttrs (map (mkGitHash toPackageIdForImportCargoLock) unhashedGitDeps);

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
      importCargoLock = import ./importCargoLock.nix;

      hashes = outputHashes crateDir;
      extraHashesForImportCargoLock = builtins.trace hashes.extraHashesForImportCargoLock hashes.extraHashesForImportCargoLock;
      extraHashes = hashes.extraHashes;
      # this downloads all of our build dependencies (rust) and stores them locally in /nix/store
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

        # we need to propagate CARGO_HOME with all of our dependencies
        export CARGO_HOME="$out/cargo"
        mkdir -p $CARGO_HOME
        cp -r ${vendoredCargoConfig} $CARGO_HOME/config
        cp ${vendoredCargoLock}/Cargo.lock $out/Cargo.lock
        ln -s ${vendoredCargoLock} $out/cargo-vendor-dir
        export HOME="$out"

        # we calculate hashes of all of the dependencies
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

  # allows to propagate downloaded crates to other derivations
  vendoredCargoLock = src: cargoToml:
    let
      crateDir = dirOf (src + "/${cargoToml}");
      cargoLock = builtins.readFile (src + "/Cargo.lock");
      importCargoLock = import ./importCargoLock.nix;
      extraHashesForImportCargoLock = (outputHashes crateDir).extraHashesForImportCargoLock;
    in
    importCargoLock { lockFileContents = cargoLock; outputHashes = extraHashesForImportCargoLock; };
}

{ pkgs
, lib ? pkgs.lib
, importCargoLock
}:
let
  outputHashes = lockFile:
    let
      lockFileContent = (builtins.fromTOML (builtins.readFile lockFile)).package or [];

      toPackageId = { name, version, source, ... }:
              "${name} ${version} (${source})";

      toPackageIdForImportCargoLock = { name, version, ... }:
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
          gitSource = parseGitSource source;
          src = builtins.fetchGit {
            submodules = true;
            inherit (gitSource) url rev;
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
          packagesWithoutLocal = builtins.filter (p: p ? source) lockFileContent;
          packageById = package: { name = toPackageId package; value = package; };
          # it removes possible duplicates (it takes first occurrence)
          packagesById = builtins.listToAttrs (builtins.map packageById packagesWithoutLocal);
        in
        builtins.attrValues packagesById;

      gitPackages = builtins.filter isGitSource packages;

      packageToNamedHash = toPackageIdFun: package: { name = toPackageIdFun package; value = mkGitHash package; };

      extraHashesForImportCargoLock = builtins.listToAttrs (map (packageToNamedHash toPackageIdForImportCargoLock) gitPackages);
  in
  extraHashesForImportCargoLock;
in
rec {
/* allows to propagate downloaded crates to other derivations
     src: the source that is needed to build the crate, usually the
     crate/workspace root directory
     cargoLock: path to the Cargo.lock file relative to src
  */
  vendoredCargoLock = cargoLock:
    let
      crateDir = dirOf cargoLock;
      lockFileContents = builtins.readFile cargoLock;
      extraHashesForImportCargoLock = outputHashes cargoLock;
    in
    importCargoLock { inherit lockFileContents; outputHashes = extraHashesForImportCargoLock; };

  flakeCompat = fetchTarball {
    url = "https://github.com/edolstra/flake-compat/archive/12c64ca55c1014cdc1b16ed5a804aa8576601ff2.tar.gz";
    sha256 = "0jm6nzb83wa6ai17ly9fzpqc40wg1viib8klq8lby54agpl213w5";
  };

  importCargo =
    let
      importCargoSrc = builtins.fetchTarball {
        url = "https://github.com/edolstra/import-cargo/archive/25d40be4a73d40a2572e0cc233b83253554f06c5.tar.gz";
        sha256 = "0dnwaz58s7pcfdvwi0crmx8cqlxi7il623n126db9nkq0fp41fvy";
      };
      importCargo = (import flakeCompat { src = importCargoSrc; }).defaultNix;
    in
    importCargo;
}

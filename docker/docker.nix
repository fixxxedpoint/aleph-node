let
  nixpkgs = import (builtins.fetchTarball {
    url = "https://github.com/NixOS/nixpkgs/archive/be872a7453a176df625c12190b8a6c10f6b21647.tar.gz";
    sha256 = "1hnwh2w5rhxgbp6c8illcrzh85ky81pyqx9309bkgpivyzjf2nba";
  }) {};

  nixFromDockerHub = nixpkgs.dockerTools.pullImage {
    imageName = "nixos/nix";
    imageDigest = "sha256:85299d86263a3059cf19f419f9d286cc9f06d3c13146a8ebbb21b3437f598357";
    sha256 = "19fw0n3wmddahzr20mhdqv6jkjn1kanh6n2mrr08ai53dr8ph5n7";
    finalImageTag = "2.6.0";
    finalImageName = "nix";
  };

  alephNodeDrv = import ../nix/aleph-node.nix {};
  alephNode = alephNodeDrv.project.workspaceMembers."aleph-node".build;
  toolsDependencies = [ alephNodeDrv.generated.buildInputs alephNodeDrv.generated.nativeBuildInputs];
  allBuildDeps = deps: deps ++ (builtins.concatMap (dep: dep.buildInputs ++ dep.nativeBuildInputs) deps);
  buildDependencies = nixpkgs.lib.unique (
    (allBuildDeps alephNode.completeDeps) ++
    (allBuildDeps alephNode.completeBuildDeps) ++
    alephNode.nativeBuildInputs ++
    alephNode.buildInputs ++
    [alephNode.stdenv.cc] ++
    toolsDependencies ++
    [nixpkgs.bash]);
in
nixpkgs.dockerTools.buildImage {
  name = "aleph_build_image";
  contents = [buildDependencies];
  fromImage = nixFromDockerHub;
}

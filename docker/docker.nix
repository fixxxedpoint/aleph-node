let
  nixpkgs = import (builtins.fetchTarball {
    url = "https://github.com/NixOS/nixpkgs/archive/be872a7453a176df625c12190b8a6c10f6b21647.tar.gz";
    sha256 = "1hnwh2w5rhxgbp6c8illcrzh85ky81pyqx9309bkgpivyzjf2nba";
  }) {};

  nixFromDockerHub = nixpkgs.dockerTools.pullImage {
    imageName = "nixos/nix";
    imageDigest = "sha256:f0c68f870c655d8d96658ca762a0704a30704de22d16b4956e762a2ddfbccb09";
    sha256 = "sha256-yHjUZkw/QwZ/L0nMWg19qSqJCFwo40LjOOs5+eKkNgM=";
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
  fromImageName = "nixos/nix";
  fromImageTag = "2.6.0";
}

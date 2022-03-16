let
  versions = import ../nix/versions.nix;
  nixpkgs = versions.dockerNixpkgs;

  fetchDependenciesAttrs = nixpkgs.lib.filterAttrs (n: _: nixpkgs.lib.hasPrefix "fetch" n) versions;
  fetchDependencies = nixpkgs.lib.mapAttrsToList (_: v: v) fetchDependenciesAttrs;

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
    fetchDependencies ++
    toolsDependencies ++
    [nixpkgs.bash]);
in
nixpkgs.dockerTools.buildImage {
  name = "aleph_build_image";
  contents = [buildDependencies];
  fromImage = nixFromDockerHub;
  fromImageName = "nix";
  fromImageTag = "2.6.0";
}

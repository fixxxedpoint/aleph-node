let
  nixpkgs = import (builtins.fetchTarball {
    url = "https://github.com/NixOS/nixpkgs/archive/be872a7453a176df625c12190b8a6c10f6b21647.tar.gz";
    sha256 = "1hnwh2w5rhxgbp6c8illcrzh85ky81pyqx9309bkgpivyzjf2nba";
  }) {};

  alephNodeDrv = import ../nix/aleph-node.nix {};
  alephNode = alephNodeDrv.project.workspaceMembers."aleph-node".build;
  toolsDependencies = [ alephNodeDrv.generated.buildInputs alephNodeDrv.generated.nativeBuildInputs];
  buildDependencies = nixpkgs.lib.unique (
    alephNode.completeDeps ++
    alephNode.completeBuildDeps ++
    alephNode.nativeBuildInputs ++
    alephNode.buildInputs ++
    alephNode.depsBuildBuild ++
    toolsDependencies ++
    [nixpkgs.nix_2_6]);
in
nixpkgs.dockerTools.buildImage {
  name = "aleph_build_image";
  contents = [ buildDependencies ];
}

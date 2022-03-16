let
  versions = import ../nix/versions.nix;
  nixpkgs = versions.dockerNixpkgs;

  alephNodeDrv = import ../nix/aleph-node.nix {};
  alephNode = alephNodeDrv.project.workspaceMembers."aleph-node".build;
  buildDependencies = nixpkgs.runCommand "aleph-node dependencies" { exportReferencesGraph = [ "deps" alephNode.drvPath ]; } ''cp deps $out'';

  nixFromDockerHub = nixpkgs.dockerTools.pullImage {
    imageName = "nixos/nix";
    imageDigest = "sha256:f0c68f870c655d8d96658ca762a0704a30704de22d16b4956e762a2ddfbccb09";
    sha256 = "sha256-pje9GziBsB28BDzhNSnrphN6xzxxsEnP2YDp7zAED8o=";
    finalImageTag = "2.6.0";
    finalImageName = "nixos/nix";
  };

in
nixpkgs.dockerTools.buildImage {
  name = "aleph_build_image";
  contents = [buildDependencies];
  fromImage = nixFromDockerHub;
  fromImageName = "nixos/nix";
  fromImageTag = "2.6.0";
}

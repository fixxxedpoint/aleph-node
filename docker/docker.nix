let
  versions = import ../nix/versions.nix;
  nixpkgs = versions.dockerNixpkgs;
  mainNixpkgs = versions.mainNixpkgs;
  nixFromDockerHub = nixpkgs.dockerTools.pullImage {

    imageName = "nixos/nix";
    imageDigest = "sha256:f0c68f870c655d8d96658ca762a0704a30704de22d16b4956e762a2ddfbccb09";
    sha256 = "sha256-pje9GziBsB28BDzhNSnrphN6xzxxsEnP2YDp7zAED8o=";
    finalImageTag = "2.6.0";
    finalImageName = "nixos/nix";
  };

  alephNodeDrv = import ../nix/aleph-node.nix {};
  alephNode = alephNodeDrv.project.workspaceMembers."aleph-node".build;
  alephNodeSrc = nixpkgs.copyPathToStore ../.;
  dockerEntrypointScript = nixpkgs.writeScriptBin "docker-entrypoint.sh" (builtins.readFile ./docker_entrypoint.sh);

  alephBuildImage = nixpkgs.dockerTools.buildImage {
    name = "aleph_build_image";
    contents = [alephNodeSrc mainNixpkgs.patchelf];
    extraCommands = ''
      nix-collect-garbage
      cd ${alephNodeSrc}
      nix-build
    '';
    fromImage = nixFromDockerHub;
    fromImageName = "nixos/nix";
    fromImageTag = "2.6.0";
  };

  alephNodeImage = nixpkgs.dockerTools.buildImage {
    name = "aleph-node";
    contents = [alephNode dockerEntrypointScript mainNixpkgs.bash mainNixpkgs.coreutils mainNixpkgs.cacert];
    config = {
      Env = [
        "PATH=${alephNode}/bin:${mainNixpkgs.bash}/bin:${mainNixpkgs.coreutils}/bin"
      ];
      Entrypoint = "${dockerEntrypointScript}/bin/docker-entrypoint.sh";
      ExposedPorts = {
        "30333" = {};
        "9933" = {};
        "9944" = {};
      };
    };
  };
in
{
  inherit alephNodeImage alephBuildImage;
}

let
  versions = import ../nix/versions.nix;
  nixpkgs = versions.dockerNixpkgs;
  nixFromDockerHub = nixpkgs.dockerTools.pullImage {

    imageName = "nixos/nix";
    imageDigest = "sha256:f0c68f870c655d8d96658ca762a0704a30704de22d16b4956e762a2ddfbccb09";
    sha256 = "sha256-pje9GziBsB28BDzhNSnrphN6xzxxsEnP2YDp7zAED8o=";
    finalImageTag = "2.6.0";
    finalImageName = "nixos/nix";
  };

  alephNodeDrv = import ../nix/aleph-node.nix {};
  alephNode = alephNodeDrv.project.workspaceMembers."aleph-node".build;
  alephNodeSrc = ../.;

  alephBuildImage = nixpkgs.dockerTools.buildImage {
    name = "aleph_build_image";
    contents = [alephNodeSrc];
    extraCommands = ''
      cd ${alephNodeSrc}
      nix-build
    '';
    fromImage = nixFromDockerHub;
    fromImageName = "nixos/nix";
    fromImageTag = "2.6.0";
  };

  alephNodeImage = nixpkgs.dockerTools.buildImage {
    name = "aleph-node";
    contents = [alephNode alephNodeSrc nixpkgs.bash nixpkgs.coreutils];
    extraCommands = ''
      mkdir -p /node
      chmod +w /node
      cp "${alephNodeSrc}/docker/docker_entrypoint.sh" /node
      chmod +x /node/docker_entrypoint.sh
    '';
    config = {
      WorkingDir = "/node";
      Entrypoint = "./docker_entrypoint.sh";
      ExposedPorts = {
        "30333" = {};
        "9933" = {};
        "9944" = {};
      };
    };
  };
in
alephNodeImage

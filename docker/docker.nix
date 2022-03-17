let
  versions = import ../nix/versions.nix;
  nixpkgs = versions.dockerNixpkgs;
  mainNixpkgs = versions.mainNixpkgs;

  alephNodeDrv = import ../nix/aleph-node.nix {};
  alephNode = alephNodeDrv.project.workspaceMembers."aleph-node".build;
  alephNodeSrc = alephNodeDrv.src;
  dockerEntrypointScript = nixpkgs.writeScriptBin "docker-entrypoint.sh" (builtins.readFile ./docker_entrypoint.sh);

  alephNodeImage = nixpkgs.dockerTools.buildImage {
    name = "aleph-node";
    created = "now";
    contents = [alephNode alephNodeSrc dockerEntrypointScript mainNixpkgs.bash mainNixpkgs.coreutils mainNixpkgs.cacert];
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
alephNodeImage

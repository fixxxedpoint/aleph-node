let
  versions = import ../nix/versions.nix;
  nixpkgs = versions.dockerNixpkgs;
  mainNixpkgs = versions.mainNixpkgs;

  alephNodeDrv = import ../nix/aleph-node.nix {};
  alephNode = alephNodeDrv.project.workspaceMembers."aleph-node".build;
  alephNodeSrc = nixpkgs.runCommand "aleph-node.src" {} ''
    mkdir -p $out
    tar cfa $out/aleph-node.src.tar.gz ${alephNodeDrv.src}
  '';
  dockerEntrypointScript = (nixpkgs.writeScriptBin "docker_entrypoint.sh" (builtins.readFile ./docker_entrypoint.sh)).overrideAttrs(old: {
    buildCommand = "${old.buildCommand}\n patchShebangs $out";
  });

  alephNodeImage = nixpkgs.dockerTools.buildImage {
    name = "aleph-node";
    created = "now";
    contents = [alephNode alephNodeSrc dockerEntrypointScript mainNixpkgs.bash mainNixpkgs.coreutils mainNixpkgs.cacert];
    config = {
      Env = [
        "PATH=${alephNode}/bin:${dockerEntrypointScript}/bin:${mainNixpkgs.bash}/bin:${mainNixpkgs.coreutils}/bin"
      ];
      Entrypoint = "${dockerEntrypointScript}/bin/docker_entrypoint.sh";
      ExposedPorts = {
        "30333" = {};
        "9933" = {};
        "9944" = {};
      };
    };
  };
in
alephNodeImage

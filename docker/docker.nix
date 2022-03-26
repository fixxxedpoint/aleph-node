# defines a derivation that builds a minimal docker image containing aleph-node and its src folder
{ versions ? import ../nix/versions.nix {}
, nixpkgs ? versions.nixpkgs
, nixpkgsForDocker ? versions.dockerNixpkgs
}:
let
    # default set of cpu features for x86-64-v3 (rustc --print=cfg -C target-cpu=x86-64-v3)
  targetFeatures = [ "avx" "avx2" "bmi1" "bmi2" "fma" "fxsr" "lzcnt" "popcnt" "sse" "sse2" "sse3" "sse4.1" "sse4.2" "ssse3" "xsave" ];
  alephNodeDrv = import ../nix/aleph-node.nix { inherit targetFeatures; };
  alephNode = alephNodeDrv.project.workspaceMembers."aleph-node".build;
  # we include gziped src folder
  alephNodeSrc = nixpkgs.runCommand "aleph-node.src" {} ''
    mkdir -p $out
    tar cfa $out/aleph-node.src.tar.gz ${alephNodeDrv.src}
  '';
  dockerEntrypointScript = (nixpkgs.writeScriptBin "docker_entrypoint.sh" (builtins.readFile ./docker_entrypoint.sh)).overrideAttrs(old: {
    buildCommand = ''
      ${old.buildCommand}
      # fixes #! /usr/bin/env bash preamble
      patchShebangs $out
    '';
  });

  alephNodeImage = nixpkgsForDocker.dockerTools.buildImage {
    name = "aleph-node";
    created = "now";
    contents = [alephNode alephNodeSrc dockerEntrypointScript nixpkgs.bash nixpkgs.coreutils nixpkgs.cacert];
    config = {
      Env = [
        "PATH=${alephNode}/bin:${dockerEntrypointScript}/bin:${nixpkgs.bash}/bin:${nixpkgs.coreutils}/bin"
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

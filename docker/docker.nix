let
  nixpkgs = import (builtins.fetchGit {
    url = "https://github.com/NixOS/nixpkgs/";
    ref = "refs/tags/nixos-21.11";
  });

  # use newest nixpkgs instead of that version
  alephNode = (import ../nix/aleph-node.nix {}).workspaceMembers."aleph-node".build;
  buildDependencies = nixpkgs.lib.unique (alephNode.completeDeps ++ alephNode.completeBuildDeps ++ alephNode.nativeBuildInputs ++ alephNode.buildInputs);
in
nixpkgs.dockerTools.streamLayeredImage {
  name = "aleph_build_image";
  contents = [ buildDependencies ];
}

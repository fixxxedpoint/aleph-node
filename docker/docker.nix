{ pkgs ? import ../nix/nixpkgs.nix {}
}:
let
  # use newest nixpkgs instead of that version
  alephNode = (import ../nix/aleph-node.nix {}).workspaceMembers."aleph-node".build;
  buildDependencies = pkgs.lib.unique (alephNode.completeDeps ++ alephNode.completeBuildDeps ++ alephNode.nativeBuildInputs ++ alephNode.buildInputs);
in
pkgs.dockerTools.streamLayeredImage {
  name = "aleph_build_image";
  contents = [ buildDependencies ];
}

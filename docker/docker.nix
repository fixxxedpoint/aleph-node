{ pkgs ? import ../nix/nixpkgs.nix {}
}:
let
  alephNode = import ../default.nix {};
  buildDependencies = pkgs.lib.unique (alephNode.completeDeps ++ alephNode.completeBuildDeps);
in
pkgs.dockerTools.streamLayeredImage {
  name = "aleph_build_image";
  contents = [ buildDependencies ];
}

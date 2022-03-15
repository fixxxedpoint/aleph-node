let
  nixpkgs = import (builtins.fetchTarball {
    url = "https://github.com/NixOS/nixpkgs/archive/a7ecde854aee5c4c7cd6177f54a99d2c1ff28a31.tar.gz";
    sha256 = "162dywda2dvfj1248afxc45kcrg83appjd0nmdb541hl7rnncf02";
  }) {};

  # use newest nixpkgs instead of that version
  alephNode = (import ../nix/aleph-node.nix {}).workspaceMembers."aleph-node".build;
  buildDependencies = nixpkgs.lib.unique (alephNode.completeDeps ++ alephNode.completeBuildDeps ++ alephNode.nativeBuildInputs ++ alephNode.buildInputs);
in
nixpkgs.dockerTools.streamLayeredImage {
  name = "aleph_build_image";
  contents = [ buildDependencies ];
}

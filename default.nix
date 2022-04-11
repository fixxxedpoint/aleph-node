let
  flakeCompat = (import ./nix/tools.nix).flake-compat;
in
(flakeCompat { src = ./.; }).defaultNix

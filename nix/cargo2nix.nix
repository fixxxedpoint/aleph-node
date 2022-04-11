let
  flakeCompat = (import ./tools.nix).flake-compat;
  cargo2nixSrc = builtins.fetchTarball {
    url = "https://github.com/cargo2nix/cargo2nix/archive/refs/tags/v0.10.0.tar.gz";
    sha256 = "10waahf1a518h65jxxz4q688k3vimj82v2bxl7y7vv6hp4pk724r";
  };
in
# (flakeCompat { src = cargo2nixSrc; }).defaultNix
{
  inherit cargo2nixSrc;
  cargo2nix = (flakeCompat { src = cargo2nixSrc; }).defaultNix;
}

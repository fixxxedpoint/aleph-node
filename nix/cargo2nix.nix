let
  flakeCompat = (import ./tools.nix).flake-compat;
  cargo2nixSrc = builtins.fetchTarball {
    url = "https://github.com/euank/cargo2nix/archive/99a37794a865dd2645719cd62cc172facdc7017e.tar.gz";
    sha256 = "056v06bmpqkibpbrz4d35s47laz6l1jap5pccmp967l2i52zzj77";
  };
in
# (flakeCompat { src = cargo2nixSrc; }).defaultNix
{
  inherit cargo2nixSrc;
  cargo2nix = (flakeCompat { src = cargo2nixSrc; }).defaultNix;
}

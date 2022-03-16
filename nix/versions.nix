rec {
  fetchImportCargoLock = builtins.fetchTarball {
    url = "https://github.com/NixOS/nixpkgs/archive/be872a7453a176df625c12190b8a6c10f6b21647.tar.gz";
    sha256 = "1hnwh2w5rhxgbp6c8illcrzh85ky81pyqx9309bkgpivyzjf2nba";
  };

  importCargoLock = (import fetchImportCargoLock {}).rustPlatform.importCargoLock;

  fetchCrate2nix = builtins.fetchTarball {
    url = "https://github.com/NixOS/nixpkgs/archive/c82b46413401efa740a0b994f52e9903a4f6dcd5.tar.gz";
    sha256 = "13s8g6p0gzpa1q6mwc2fj2v451dsars67m4mwciimgfwhdlxx0bk";
  };
  crate2nix = (import fetchCrate2nix {}).crate2nix;

  fetchNixpkgs = (builtins.fetchTarball {
    url = "https://github.com/NixOS/nixpkgs/archive/2c162d49cd5b979eb66ff1653aecaeaa01690fcc.tar.gz";
    sha256 = "08k7jy14rlpbb885x8dyds5pxr2h64mggfgil23vgyw6f1cn9kz6";
  });

  fetchRustOverlay = builtins.fetchTarball {
    url = "https://github.com/mozilla/nixpkgs-mozilla/archive/15b7a05f20aab51c4ffbefddb1b448e862dccb7d.tar.gz";
    sha256 = "0admybxrjan9a04wq54c3zykpw81sc1z1nqclm74a7pgjdp7iqv1";
  };

  mainNixpkgs =
    let
      # this overlay allows us to use a specified version of the rust toolchain
      rustOverlay =
        import fetchRustOverlay;

      overrideRustTarget = rustChannel: rustChannel // {
        rust = rustChannel.rust.override {
          targets = [ "x86_64-unknown-linux-gnu" "wasm32-unknown-unknown" ];
        };
      };
      rustToolchain = with nixpkgs; overrideRustTarget ( rustChannelOf { rustToolchain = ../rust-toolchain; } );

      inherit crate2nix;

      # pinned version of nix packages
      nixpkgs = import fetchNixpkgs { overlays = [
            rustOverlay
            (self: super: {
              inherit (rustToolchain) cargo rust-src rust-std;
              rustc = rustToolchain.rust;

              inherit crate2nix;
            })
          ];
        };
    in
    nixpkgs;

  fetchDockerNixpkgs = builtins.fetchTarball {
    url = "https://github.com/NixOS/nixpkgs/archive/be872a7453a176df625c12190b8a6c10f6b21647.tar.gz";
    sha256 = "1hnwh2w5rhxgbp6c8illcrzh85ky81pyqx9309bkgpivyzjf2nba";
  };
  dockerNixpkgs = import fetchDockerNixpkgs {};
}

let
  mozillaOverlay =
    import (builtins.fetchGit {
      url = "https://github.com/mozilla/nixpkgs-mozilla.git";
      rev = "f233fdc4ff6ba2ffeb1e3e3cd6d63bb1297d6996";
    });
  nixpkgs = import <nixpkgs> { overlays = [ mozillaOverlay ]; };
  rust-nightly = with nixpkgs; ((rustChannelOf { date = "2021-10-24"; channel = "nightly"; }).rust.override {
    extensions = [ "rust-src" ];
    targets = [ "wasm32-unknown-unknown" ];
  });
  binutils-unwrapped' = nixpkgs.binutils-unwrapped.overrideAttrs (old: {
    name = "binutils-2.37";
    src = nixpkgs.fetchurl {
      url = "https://ftp.gnu.org/gnu/binutils/binutils-2.37.tar.xz";
      sha256 = "sha256-gg2XJPAgo+acszeJOgtjwtsWHa3LDgb8Edwp6x6Eoyw=";
    };
    patches = [];
  });
  env = nixpkgs.llvmPackages_12.stdenv;
  cc = nixpkgs.wrapCCWith rec {
    cc = env.cc;
    bintools = nixpkgs.wrapBintoolsWith {
      bintools = binutils-unwrapped';
    };
  };
  minimalMkShell = nixpkgs.mkShell.override {
    # stdenv = nixpkgs.stdenvNoCC;
    # stdenv = nixpkgs.clangStdenv;
    stdenv = nixpkgs.overrideCC env cc;
  };
in
with nixpkgs; minimalMkShell {
  buildInputs = [
    clang_12
    openssl.dev
    pkg-config
    rust-nightly
    cacert
    protobuf
  ] ++ lib.optionals stdenv.isDarwin [
    darwin.apple_sdk.frameworks.Security
  ];

  RUST_SRC_PATH = "${rust-nightly}/lib/rustlib/src/rust/src";
  LIBCLANG_PATH = "${llvmPackages.libclang.lib}/lib";
  PROTOC = "${protobuf}/bin/protoc";
}
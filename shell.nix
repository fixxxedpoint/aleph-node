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
  # binutils-unwrapped' = nixpkgs.binutils-unwrapped.overrideAttrs (old: {
  #   name = "binutils-2.36.1";
  #   src = nixpkgs.fetchurl {
  #     url = "https://ftp.gnu.org/gnu/binutils/binutils-2.36.1.tar.xz";
  #     sha256 = "e81d9edf373f193af428a0f256674aea62a9d74dfe93f65192d4eae030b0f3b0";
  #   };
  #   patches = [];
  # });
  # env = nixpkgs.llvmPackages_12.stdenv;
  # cc = nixpkgs.wrapCCWith rec {
  #   cc = env.cc;
  #   bintools = nixpkgs.wrapBintoolsWith {
  #     bintools = binutils-unwrapped';
  #     libc = env.cc.bintools.libc;
  #   };
  # };
  # minimalMkShell = nixpkgs.mkShell.override {
  #   # stdenv = nixpkgs.stdenvNoCC;
  #   # stdenv = nixpkgs.clangStdenv;
  #   stdenv = nixpkgs.overrideCC env cc;
  # };
in
# with nixpkgs; minimalMkShell {
with nixpkgs; mkShell {
  buildInputs = [
    clang_12
    # libcxx
    # libcxxabi
    # llvm_12
    # llvmPackages.libcxxClang
    # llvmPackages.libcxxStdenv
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
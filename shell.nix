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
    name = "binutils-2.36.1";
    src = nixpkgs.fetchurl {
      url = "https://ftp.gnu.org/gnu/binutils/binutils-2.36.1.tar.xz";
      sha256 = "e81d9edf373f193af428a0f256674aea62a9d74dfe93f65192d4eae030b0f3b0";
    };
    patches = [];
  });
  env = nixpkgs.llvmPackages_12.stdenv;
  cc = nixpkgs.wrapCCWith rec {
    cc = nixpkgs.llvmPackages_12.clang-unwrapped;
    bintools = nixpkgs.wrapBintoolsWith {
      bintools = binutils-unwrapped';
    };
  };
  minimalMkShell = with nixpkgs; pkgs.mkShell.override {
    stdenv = nixpkgs.overrideCC env cc;
  };
in
with nixpkgs; minimalMkShell {

  buildInputs = [
    llvmPackages_12.clang
    openssl.dev
    pkg-config
    rust-nightly
    cacert
    protobuf
  ];

  RUST_SRC_PATH = "${rust-nightly}/lib/rustlib/src/rust/src";
  LIBCLANG_PATH = "${llvmPackages_12.libclang.lib}/lib";
  PROTOC = "${protobuf}/bin/protoc";
  CFLAGS=" \
      ${"-isystem ${llvmPackages_12.libclang.lib}/lib/clang/${lib.getVersion llvmPackages_12.stdenv.cc.cc}/include"}
  ";
  CXXFLAGS=" \
      ${"-isystem ${llvmPackages_12.libclang.lib}/lib/clang/${lib.getVersion llvmPackages_12.stdenv.cc.cc}/include"}
  ";

  # From: https://github.com/NixOS/nixpkgs/blob/1fab95f5190d087e66a3502481e34e15d62090aa/pkgs/applications/networking/browsers/firefox/common.nix#L247-L253
  # Set C flags for Rust's bindgen program. Unlike ordinary C
  # compilation, bindgen does not invoke $CC directly. Instead it
  # uses LLVM's libclang. To make sure all necessary flags are
  # included we need to look in a few places.
  BINDGEN_EXTRA_CLANG_ARGS=" \
      ${"-isystem ${llvmPackages_12.libclang.lib}/lib/clang/${lib.getVersion llvmPackages_12.stdenv.cc.cc}/include"}
  ";
}

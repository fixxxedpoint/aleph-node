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
  # env = nixpkgs.llvmPackages_12.stdenv;
  # env = nixpkgs.llvmPackages_12.libcxxStdenv;
  # env = nixpkgs.llvmPackages_12.clang12Stdenv;
  env = nixpkgs.clangStdenv;
  # env = nixpkgs.llvmPackages_12.stdenv;
  # env = nixplgs.libcxxStdenv
  # env = nixpkgs.stdenv;
  cc = nixpkgs.wrapCCWith rec {
    cc = env.cc;
    # cc = "cc";
    bintools = nixpkgs.wrapBintoolsWith {
      bintools = binutils-unwrapped';
      # bintools = env.cc.bintools.bintools;
      libc = env.cc.bintools.libc;
    };
  };
  minimalMkShell = nixpkgs.mkShell.override {
    # stdenv = nixpkgs.stdenvNoCC;
    # stdenv = nixpkgs.clangStdenv;
    stdenv = nixpkgs.overrideCC env cc;

  };
in
with nixpkgs; minimalMkShell {
# with nixpkgs; mkShell {
  buildInputs = [
    # clang_12
    # zstd
    # libcxx
    # libcxxabi
    # llvm_12
    # llvmPackages.libcxxClang
    # llvmPackages.libcxxStdenv
    # llvmPackages_12.clang
    # llvmPackages_12.libclang
    # llvmPackages_12.llvm
    openssl.dev
    pkg-config
    rust-nightly
    cacert
    protobuf
  ] ++ lib.optionals stdenv.isDarwin [
    darwin.apple_sdk.frameworks.Security
  ];

  # shellHook = ''
  #   export CC=clang-12;
  #   export LD_LIBRARY_PATH="${llvmPackages_12.libclang.lib}/lib";
  # '';


  # RUST_SRC_PATH = "${rust-nightly}/lib/rustlib/src/rust/src";
  # LIBCLANG_PATH = "${llvmPackages_12.libclang.lib}/lib";
  # PROTOC = "${protobuf}/bin/protoc";
  # CC = "clang";
  # CXX = "clang";
}
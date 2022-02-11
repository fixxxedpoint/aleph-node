let
  mozillaOverlay =
    import (builtins.fetchGit {
      url = "https://github.com/mozilla/nixpkgs-mozilla.git";
      rev = "f233fdc4ff6ba2ffeb1e3e3cd6d63bb1297d6996";
    });
  nixpkgs = import <nixpkgs> { overlays = [ mozillaOverlay ]; };
  rust-nightly = with nixpkgs; ((rustChannelOf { date = "2021-10-24"; channel = "nightly"; }).rust.override {
    extensions = [ "rust-src" ];
    targets = [ "x86_64-unknown-linux-gnu", "wasm32-unknown-unknown" ];
  });
  # binutils-unwrapped' = nixpkgs.binutils-unwrapped.overrideAttrs (old: {
  #   name = "binutils-2.36.1";
  #   src = nixpkgs.fetchurl {
  #     url = "https://ftp.gnu.org/gnu/binutils/binutils-2.36.1.tar.xz";
  #     sha256 = "e81d9edf373f193af428a0f256674aea62a9d74dfe93f65192d4eae030b0f3b0";
  #   };
  #   patches = [];
  # });
  llvm = nixpkgs.llvmPackages_13;
  env = llvm.stdenv;
  # cc = nixpkgs.wrapCCWith rec {
  #   cc = env.cc;
  #   bintools = nixpkgs.wrapBintoolsWith {
  #     bintools = binutils-unwrapped';
  #   };
  # };
  # customEnv = nixpkgs.overrideCC env cc;
  customEnv = env;
in
with nixpkgs; customEnv.mkDerivation rec {
  name = "aleph-node";
  src = ./.;

  buildInputs = [
    llvm.clang
    # binutils-unwrapped'
    llvm.lld
    openssl.dev
    pkg-config
    rust-nightly
    cacert
    protobuf
  ];

  shellHook = ''
    # export CARGO_HOME="$out/cargo"
    export RUST_SRC_PATH="${rust-nightly}/lib/rustlib/src/rust/src"
    export LIBCLANG_PATH="${llvm.libclang.lib}/lib"
    export PROTOC="${protobuf}/bin/protoc"
    export CFLAGS=" \
        ${"-isystem ${llvm.libclang.lib}/lib/clang/${lib.getVersion llvm.stdenv.cc.cc}/include"} \
        $CFLAGS
    "
    export CXXFLAGS+=" \
        ${"-isystem ${llvm.libclang.lib}/lib/clang/${lib.getVersion llvm.stdenv.cc.cc}/include"} \
        $CXXFLAGS
    "
    # From: https://github.com/NixOS/nixpkgs/blob/1fab95f5190d087e66a3502481e34e15d62090aa/pkgs/applications/networking/browsers/firefox/common.nix#L247-L253
    # Set C flags for Rust's bindgen program. Unlike ordinary C
    # compilation, bindgen does not invoke $CC directly. Instead it
    # uses LLVM's libclang. To make sure all necessary flags are
    # included we need to look in a few places.
    export BINDGEN_EXTRA_CLANG_ARGS=" \
        ${"-isystem ${llvm.libclang.lib}/lib/clang/${lib.getVersion llvm.stdenv.cc.cc}/include"} \
        $BINDGEN_EXTRA_CLANG_ARGS
    "
    # export RUSTFLAGS="-C linker=clang -C link-arg=-fuse-ld=lld -C target-cpu=cascadelake $RUSTFLAGS"
    # export RUSTFLAGS="-C link-arg=-fuse-ld=lld -C target-cpu=cascadelake $RUSTFLAGS"
    # export RUSTFLAGS="-C linker=lld -C target-cpu=cascadelake $RUSTFLAGS"
    # export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER="lld"
    # export RUSTFLAGS="-C target-cpu=cascadelake $RUSTFLAGS"
  '';

  buildPhase = ''
    ${shellHook}

    cargo build -vv --release -p aleph-node
  '';

  installPhase = ''
    mkdir -p $out/bin
    mv -r target/ $out/
  '';
}

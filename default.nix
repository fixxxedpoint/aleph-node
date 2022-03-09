{ rocksDBVersion ? "6.29.3", release ? false }:
let
  # this overlay allows us to use a specified version of the rust toolchain
  rustOverlay =
    import (builtins.fetchGit {
      url = "https://github.com/mozilla/nixpkgs-mozilla.git";
      rev = "15b7a05f20aab51c4ffbefddb1b448e862dccb7d";
    });

  overrideRustTarget = rustChannel: rustChannel // {
    rust = rustChannel.rust.override {
      targets = [ "x86_64-unknown-linux-gnu" "wasm32-unknown-unknown" ];
    };
  };
  # use Rust toolchain declared by the rust-toolchain file
  rustToolchain = with nixpkgs; overrideRustTarget ( rustChannelOf { rustToolchain = ./rust-toolchain; } );

  # pinned version of nix packages
  nixpkgs = import (builtins.fetchTarball {
    url = "https://github.com/NixOS/nixpkgs/archive/2c162d49cd5b979eb66ff1653aecaeaa01690fcc.tar.gz";
    sha256 = "08k7jy14rlpbb885x8dyds5pxr2h64mggfgil23vgyw6f1cn9kz6";
  }) { overlays = [
         rustOverlay
         (self: super: {
           inherit (rustToolchain) cargo rust-src rust-std;
           rustc = rustToolchain.rust;
         })
       ];
     };

  # declares a build environment where C and C++ compilers are delivered by the llvm/clang project
  # in this version build process should rely only on clang, without access to gcc
  llvm = nixpkgs.llvmPackages_11;
  env = llvm.stdenv;
  llvmVersionString = "${nixpkgs.lib.getVersion env.cc.cc}";

  # we use a newer version of rocksdb than the one provided by nixpkgs
  # we disable all compression algorithms and force it to use SSE 4.2 cpu instruction set
  customRocksdb = nixpkgs.rocksdb.overrideAttrs (_: {

    src = builtins.fetchGit {
      url = "https://github.com/facebook/rocksdb.git";
      ref = "refs/tags/v${rocksDBVersion}";
    };

    version = "${rocksDBVersion}";

    patches = [];

    cmakeFlags = [
       "-DPORTABLE=0"
       "-DWITH_JNI=0"
       "-DWITH_BENCHMARK_TOOLS=0"
       "-DWITH_TESTS=0"
       "-DWITH_TOOLS=0"
       "-DWITH_BZ2=0"
       "-DWITH_LZ4=0"
       "-DWITH_SNAPPY=0"
       "-DWITH_ZLIB=0"
       "-DWITH_ZSTD=0"
       "-DWITH_GFLAGS=0"
       "-DUSE_RTTI=0"
       "-DFORCE_SSE42=1"
       "-DROCKSDB_BUILD_SHARED=0"
    ];

    propagatedBuildInputs = [];

    buildInputs = [ nixpkgs.git ];
  });

  sources = import ./nix/sources.nix;
  naersk = nixpkgs.callPackage sources.naersk { stdenv = env; };
in
with nixpkgs; naersk.buildPackage {
  name = "aleph-node";
  src = ./.;
  nativeBuildInputs = [
    cacert
    git
  ];
  buildInputs = [
    openssl.dev
    protobuf
    pkg-config
    llvm.clang
    llvm.libclang
    customRocksdb
  ];
  compressTarget=true;
  release=release;
  override=old: {
    postConfigure=''
      # this is needed so cargo/rust doesn't rebuild all of the dependencies
      # without it, its fingerprinting complains about mtime, about forces everything to rebuild
      find . -type f -exec touch -cfht 197001010000 {} +
      find target -type f -exec touch -cfht 197001010001 {} +
      chmod +w -R target
      cargo clean -p aleph-node
    '';
  };
  cargoBuildOptions = x: x ++ [ "-p" "aleph-node" ];

  RUSTFLAGS="-C target-cpu=x86-64-v3";
  ROCKSDB_LIB_DIR="${customRocksdb}/lib";
  ROCKSDB_STATIC=1;
  LIBCLANG_PATH="${llvm.libclang.lib}/lib";
  PROTOC="${protobuf}/bin/protoc";
  # From: https://github.com/NixOS/nixpkgs/blob/1fab95f5190d087e66a3502481e34e15d62090aa/pkgs/applications/networking/browsers/firefox/common.nix#L247-L253
  # Set C flags for Rust's bindgen program. Unlike ordinary C
  # compilation, bindgen does not invoke $CC directly. Instead it
  # uses LLVM's libclang. To make sure all necessary flags are
  # included we need to look in a few places.
  BINDGEN_EXTRA_CLANG_ARGS=" \
     ${"-isystem ${llvm.libclang.lib}/lib/clang/${llvmVersionString}/include"} \
  ";
  CFLAGS=" \
    ${"-isystem ${llvm.libclang.lib}/lib/clang/${llvmVersionString}/include"} \
  ";
  CXXFLAGS=" \
    ${"-isystem ${llvm.libclang.lib}/lib/clang/${llvmVersionString}/include"} \
  ";
}

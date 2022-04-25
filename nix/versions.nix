{ rocksDbOptions ? { version = "6.29.3";
                     useSnappy = false;
                     patchVerifyChecksum = true;
                     patchPath = ./nix/rocksdb.patch;
                     enableJemalloc = true;
                   }
}:
rec {
  rustToolchain =
    let
      # use Rust toolchain declared by the rust-toolchain file
      rustToolchain = with nixpkgs; overrideRustTarget ( rustChannelOf { rustToolchain = ../rust-toolchain; } );

      overrideRustTarget = rustChannel: rustChannel // {
        rust = rustChannel.rust.override {
          targets = [ "x86_64-unknown-linux-gnu" "wasm32-unknown-unknown" ];
        };
      };
    in
      rustToolchain;

  nixpkgs =
    let
      # this overlay allows us to use a version of the rust toolchain specified by the rust-toolchain file
      rustOverlay =
        import (builtins.fetchTarball {
          url = "https://github.com/mozilla/nixpkgs-mozilla/archive/f233fdc4ff6ba2ffeb1e3e3cd6d63bb1297d6996.tar.gz";
          sha256 = "1rzz03h0b38l5sg61rmfvzpbmbd5fn2jsi1ccvq22rb76s1nbh8i";
        });

      # pinned version of nix packages
      # main reason for not using here the newest available version at the time or writing is that this way we depend on glibc version 2.31 (Ubuntu 20.04 LTS)
      nixpkgs = import (builtins.fetchTarball {
        url = "https://github.com/NixOS/nixpkgs/archive/2c162d49cd5b979eb66ff1653aecaeaa01690fcc.tar.gz";
        sha256 = "08k7jy14rlpbb885x8dyds5pxr2h64mggfgil23vgyw6f1cn9kz6";
      }) { overlays = [
             rustOverlay
             # we override rust toolchain
             (self: super: {
               inherit (rustToolchain) cargo rust-src rust-std;
               rustc = rustToolchain.rust;
             })
           ];
         };
    in
      nixpkgs;

  llvm = nixpkgs.llvmPackages_11;

  stdenv = nixpkgs.keepDebugInfo llvm.stdenv;

  naersk =
    let
      naerskSrc = builtins.fetchTarball {
        url = "https://github.com/nix-community/naersk/archive/2fc8ce9d3c025d59fee349c1f80be9785049d653.tar.gz";
        sha256 = "1jhagazh69w7jfbrchhdss54salxc66ap1a1yd7xasc92vr0qsx4";
      };
    in
      nixpkgs.callPackage naerskSrc { inherit stdenv; cargo = rustToolchain.rust; rustc = rustToolchain.rust; };

  gitignore =
    let
      gitignoreSrc = nixpkgs.fetchFromGitHub {
        owner = "hercules-ci";
        repo = "gitignore.nix";
        rev = "5b9e0ff9d3b551234b4f3eb3983744fa354b17f1";
        sha256 = "o/BdVjNwcB6jOmzZjOH703BesSkkS5O7ej3xhyO8hAY=";
      };
    in
      import gitignoreSrc { inherit (nixpkgs) lib; };

  # we use a newer version of rocksdb than the one provided by nixpkgs
  # we disable all compression algorithms, force it to use SSE 4.2 cpu instruction set and disable its `verify_checksum` mechanism
  customRocksdb = nixpkgs.rocksdb.overrideAttrs (_: {

    src = builtins.fetchGit {
      url = "https://github.com/facebook/rocksdb.git";
      ref = "refs/tags/v${rocksDbOptions.version}";
    };

    version = "${rocksDbOptions.version}";

    patches = nixpkgs.lib.optional rocksDbOptions.patchVerifyChecksum rocksDbOptions.patchPath;

    cmakeFlags = [
       "-DPORTABLE=0"
       "-DWITH_JNI=0"
       "-DWITH_BENCHMARK_TOOLS=0"
       "-DWITH_TESTS=0"
       "-DWITH_TOOLS=0"
       "-DWITH_BZ2=0"
       "-DWITH_LZ4=0"
       "-DWITH_SNAPPY=${if rocksDbOptions.useSnappy then "1" else "0"}"
       "-DWITH_ZLIB=0"
       "-DWITH_ZSTD=0"
       "-DWITH_GFLAGS=0"
       "-DUSE_RTTI=0"
       "-DFORCE_SSE42=1"
       "-DROCKSDB_BUILD_SHARED=0"
       "-DWITH_JEMALLOC=${if rocksDbOptions.enableJemalloc then "1" else "0"}"
    ];

    propagatedBuildInputs = [];

    buildInputs = nixpkgs.lib.optionals rocksDbOptions.useSnappy [nixpkgs.snappy] ++
                  nixpkgs.lib.optionals rocksDbOptions.enableJemalloc [nixpkgs.jemalloc] ++
                 [nixpkgs.git];

    shellHook = ''
      export ROCKSDB_LIB_DIR=$out/lib
      export ROCKSDB_STATIC=1
    '';
  });
}

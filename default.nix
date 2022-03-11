{ rocksDBVersion ? "6.29.3" }:
let
  # this overlay allows us to use a specified version of the rust toolchain
  rustOverlay =
    import (builtins.fetchTarball {
      url = "https://github.com/mozilla/nixpkgs-mozilla/archive/15b7a05f20aab51c4ffbefddb1b448e862dccb7d.tar.gz";
      sha256 = "0admybxrjan9a04wq54c3zykpw81sc1z1nqclm74a7pgjdp7iqv1";
    });

  overrideRustTarget = rustChannel: rustChannel // {
    rust = rustChannel.rust.override {
      targets = [ "x86_64-unknown-linux-gnu" "wasm32-unknown-unknown" ];
    };
  };
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
           fetchGit = args: builtins.fetchGit (args // { allRefs = true; });
         })
       ];
     };

  llvm = nixpkgs.llvmPackages_11;
  env = llvm.stdenv;
  llvmVersionString = "${nixpkgs.lib.getVersion env.cc.cc}";
  buildRustCrate = nixpkgs.buildRustCrate.override {
    stdenv = env;
  };

  # we use a newer version of rocksdb than the one provided by nixpkgs
  # we disable all compression algorithms and force it to use SSE 4.2 cpu instruction set
  customRocksdb = nixpkgs.rocksdb.overrideAttrs (_: {

    src = nixpkgs.fetchGit {
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

  customBuildRustCrateForPkgs = pkgs: pkgs.buildRustCrate.override {
    stdenv = env;
    defaultCrateOverrides = pkgs.defaultCrateOverrides // (
      let protobufFix = attrs: {
            buildInputs = [ pkgs.protobuf ];
            PROTOC="${pkgs.protobuf}/bin/protoc";
          };
      in rec {
        librocksdb-sys = attrs: {
          buildInputs = [ customRocksdb ];
          ROCKSDB_LIB_DIR="${customRocksdb}/lib";
          ROCKSDB_STATIC=1;
          LIBCLANG_PATH="${llvm.libclang.lib}/lib";
        };
        libp2p-core = protobufFix;
        libp2p-plaintext = protobufFix;
        libp2p-floodsub = protobufFix;
        libp2p-gossipsub = protobufFix;
        libp2p-identify = protobufFix;
        libp2p-kad = protobufFix;
        libp2p-relay = protobufFix;
        libp2p-rendezvous = protobufFix;
        libp2p-noise = protobufFix;
        sc-network = protobufFix;
        aleph-runtime = attrs: rec {
          src = ./.;
          workspace_member = "bin/runtime";
          buildInputs = [pkgs.git pkgs.cacert];
          CARGO = "${pkgs.cargo}/bin/cargo";
          CARGO_HOME=".cargo-home";
        };
    }
    );
  };
  crate2nix = (import (builtins.fetchTarball {
    url = "https://github.com/NixOS/nixpkgs/archive/c82b46413401efa740a0b994f52e9903a4f6dcd5.tar.gz";
    sha256 = "13s8g6p0gzpa1q6mwc2fj2v451dsars67m4mwciimgfwhdlxx0bk";
  }){}).crate2nix;
  crate2nixTools = import ./tools.nix { pkgs = nixpkgs; lib = nixpkgs.lib; stdenv = env; inherit crate2nix; };
  generatedCargoNix = (crate2nixTools.generatedCargoNix {
    name = "aleph-node";
    src = ./.;
  });
  cargoNix = nixpkgs.callPackage generatedCargoNix { pkgs = nixpkgs; lib = nixpkgs.lib; buildRustCrateForPkgs = customBuildRustCrateForPkgs; };
in
cargoNix.workspaceMembers."aleph-node".build

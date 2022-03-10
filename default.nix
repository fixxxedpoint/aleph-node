{ rocksDBVersion ? "6.29.3" }:
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
  rustToolchain = with nixpkgs; overrideRustTarget ( rustChannelOf { rustToolchain = ./rust-toolchain; } );
  # rustToolchain = with nixpkgs; ( rustChannelOf { rustToolchain = ./rust-toolchain; } ).override rec {
  #   rust = rust.override {
  #     targets = [ "x86_64-unknown-linux-gnu" "wasm32-unknown-unknown" ];
  #   };
  # };
  # customRust = rustToolchain.rust.override {
  #   targets = [ "x86_64-unknown-linux-gnu" "wasm32-unknown-unknown" ];
  # };

  # pinned version of nix packages
  nixpkgs = import (builtins.fetchGit {
    url = "https://github.com/NixOS/nixpkgs/";
    ref = "refs/heads/nixpkgs-unstable";
    rev = "c82b46413401efa740a0b994f52e9903a4f6dcd5";
  }) { overlays = [
         rustOverlay
         # (import rustToolchain)
         (self: super: {
           inherit (rustToolchain) cargo rust-src rust-std;
           rustc = rustToolchain.rust;
           # with rustToolchain;
           # rustc = rustToolchain.rust;
           # rustc = customRust;
           # inherit (rustToolchain) cargo rust rust-fmt rust-std clippy;
           # import rustToolchain;
           # inherit (rustToolchain);
           # import rustToolchain;
           # rust = customRust;
         })
       ];
     };



  # # allows to skip files listed by .gitignore
  # # otherwise `nix-build` copies everything, including the target directory
  # gitignoreSrc = nixpkgs.fetchFromGitHub {
  #   owner = "hercules-ci";
  #   repo = "gitignore.nix";
  #   rev = "5b9e0ff9d3b551234b4f3eb3983744fa354b17f1";
  #   sha256 = "o/BdVjNwcB6jOmzZjOH703BesSkkS5O7ej3xhyO8hAY=";
  # };
  # inherit (import gitignoreSrc { inherit (nixpkgs) lib; }) gitignoreSource;

  # create2nixImport = import (builtins.fetchTarball {
  #   url = "https://github.com/kolloch/crate2nix/archive/refs/tags/0.10.0.tar.gz";
  #   sha256 = "aasd";
  # });
  llvm = nixpkgs.llvmPackages_11;
  env = llvm.stdenv;
  llvmVersionString = "${nixpkgs.lib.getVersion env.cc.cc}";
  buildRustCrate = nixpkgs.buildRustCrate.override {
    stdenv = env;
  };
  # crate2nix = nixpkgs.crate2nix;
  # crate2nixTools = nixpkgs.callPackage "${crate2nix.src}/tools.nix" {};
  # cargoNix = nixpkgs.callPackage (crate2nixTools.generatedCargoNix {
  #   name = "aleph-node";
  #   # src = gitignoreSource ./.;
  #   src = ./.;
  # }) { inherit buildRustCrate; };

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

  pkgs = nixpkgs;
  # cargoNix = import ./Cargo.nix { inherit pkgs; inherit buildRustCrate; };
  # cargoNix = nixpkgs.callPackage ./Cargo.nix { inherit buildRustCrate; };
  customBuildRustCrateForPkgs = pkgs: pkgs.buildRustCrate.override {
    stdenv = env;
    defaultCrateOverrides = pkgs.defaultCrateOverrides // (
      let protobufFix = attrs: {
            buildInputs = [ pkgs.protobuf ];
            PROTOC="${pkgs.protobuf}/bin/protoc";
          };
      in {
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
          preBuild = ''
            echo "hej zbyszko"
            chmod +w -R target
          '';
          buildInputs = [pkgs.git pkgs.cacert];
          CARGO = "${pkgs.cargo}/bin/cargo";
          CARGO_HOME=".cargo-home";
          # src = pkgs.lib.cleanSourceWith { filter = sourceFilter;  src = ./.; };
          src = ./.;
          sourceRoot = "${src}/bin/runtime";
          # dontMakeSourcesWritable = 1;
          # OUT_DIR="target/build/aleph-runtime";
          # CARGO_MANIFEST_DIR=".";
         #  RUST_BACKTRACE="full";
         #  BINDGEN_EXTRA_CLANG_ARGS=" \
         #    ${"-isystem ${llvm.libclang.lib}/lib/clang/${llvmVersionString}/include"} \
         #   $BINDGEN_EXTRA_CLANG_ARGS
         # ";

        };

        # aleph-node = attrs: rec {
        #   preBuild = ''rm $out/cargo/config'';
        # };
    }
    );
  };
  wrappedCrate2nix = nixpkgs.writeShellScriptBin "crate2nix" ''
    echo "wrapped crate2nix from zbyszko"
    find cargo
    rm cargo/config
    find cargo
    echo $(pwd)
    rm $out/cargo/config
    echo "$@"
    # awk -i inplace '{$1=$1};!seen[$0]++' $out/cargo/config
    exec ${nixpkgs.crate2nix}/bin/crate2nix "$@"
  '';
  # cargoNix = import ./Cargo.nix { inherit pkgs; buildRustCrateForPkgs = customBuildRustCrateForPkgs; };
  crate2nix = nixpkgs.crate2nix;
  crate2nixTools = nixpkgs.callPackage "${crate2nix.src}/tools.nix" { pkgs = nixpkgs; stdenv = env; };
  # generatedCargoNix = crate2nixTools.generatedCargoNix.overrideAttrs (old: { buildInputs = old.buildInputs ++ [ wrappedCrate2nix ]; });
  # cargoNix = nixpkgs.callPackage (crate2nixTools.generatedCargoNix {
  #   name = "aleph-node";
  #   # src = gitignoreSource ./.;
  #   src = ./.;
  # }) { buildRustCrateForPkgs = customBuildRustCrateForPkgs; };
  generatedCargoNix = (crate2nixTools.generatedCargoNix {
    name = "aleph-node";
    src = ./.;
  # }).overrideAttrs (old: { buildInputs = [ nixpkgs.cargo nixpkgs.jq wrappedCrate2nix ]; });
  }).overrideAttrs (old: { buildInputs = [wrappedCrate2nix nixpkgs.rustc nixpkgs.cacert] ++ old.buildInputs; });
  cargoNix = nixpkgs.callPackage (generatedCargoNix) { buildRustCrateForPkgs = customBuildRustCrateForPkgs; };
in
cargoNix.workspaceMembers."aleph-node".build
# cargoNixOver.workspaceMembers."aleph-runtime".build { buildRustCrateForPkgsFunc = customBuildRustCrateForPkgs; }
# cargoNix.workspaceMembers."aleph-runtime".build
# cargoNix.workspaceMembers."aleph-runtime".build.override { preBuild = ''echo "hej zbyszko 2"; chmod +w -R $out''; }

#   # declares a build environment where C and C++ compilers are delivered by the llvm/clang project
#   # in this version build process should rely only on clang, without access to gcc
#   llvm = nixpkgs.llvmPackages_11;
#   env = llvm.stdenv;
#   llvmVersionString = "${nixpkgs.lib.getVersion env.cc.cc}";
# in
# with nixpkgs; env.mkDerivation rec {
#   name = "aleph-node";
#   src = gitignoreSource ./.;

#   buildInputs = [
#     rustToolchain
#     llvm.clang
#     openssl.dev
#     protobuf
#     customRocksdb
#     pkg-config
#     cacert
#     git
#     findutils
#     patchelf
#   ];

#   shellHook = ''
#     export RUST_SRC_PATH="${rustToolchain}/lib/rustlib/src/rust/src"
#     export LIBCLANG_PATH="${llvm.libclang.lib}/lib"
#     export PROTOC="${protobuf}/bin/protoc"
#     export CFLAGS=" \
#         ${"-isystem ${llvm.libclang.lib}/lib/clang/${llvmVersionString}/include"} \
#         $CFLAGS
#     "
#     export CXXFLAGS+=" \
#         ${"-isystem ${llvm.libclang.lib}/lib/clang/${llvmVersionString}/include"} \
#         $CXXFLAGS
#     "
#     # From: https://github.com/NixOS/nixpkgs/blob/1fab95f5190d087e66a3502481e34e15d62090aa/pkgs/applications/networking/browsers/firefox/common.nix#L247-L253
#     # Set C flags for Rust's bindgen program. Unlike ordinary C
#     # compilation, bindgen does not invoke $CC directly. Instead it
#     # uses LLVM's libclang. To make sure all necessary flags are
#     # included we need to look in a few places.
#     export BINDGEN_EXTRA_CLANG_ARGS=" \
#         ${"-isystem ${llvm.libclang.lib}/lib/clang/${llvmVersionString}/include"} \
#         $BINDGEN_EXTRA_CLANG_ARGS
#     "
#     export ROCKSDB_LIB_DIR="${customRocksdb}/lib"
#     export ROCKSDB_STATIC=1
#   '';

#   buildPhase = ''
#     ${shellHook}
#     export CARGO_HOME="$out/cargo"
#     export CARGO_BUILD_TARGET="x86_64-unknown-linux-gnu"

#     cargo build --locked --release -p aleph-node
#   '';

#   installPhase = ''
#     mkdir -p $out/bin
#     mv target/x86_64-unknown-linux-gnu/release/aleph-node $out/bin/
#   '';

#   fixupPhase = ''
#     rm -rf $CARGO_HOME
#     find $out -type f -exec patchelf --shrink-rpath '{}' \; -exec strip '{}' \; 2>/dev/null
#   '';
# }

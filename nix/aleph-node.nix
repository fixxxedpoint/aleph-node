{ rocksDBVersion ? "6.29.3", nixpkgs ? import ./nixpkgs.nix }:
let
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

  crate2nix = nixpkgs.crate2nix;
  inherit (import ./tools.nix { pkgs = nixpkgs; lib = nixpkgs.lib; stdenv = env; inherit crate2nix; }) generatedCargoNix vendoredCargoLock;

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
        prost-build = protobufFix;
        aleph-runtime = attrs:
          let
            vendoredCargo = vendoredCargoLock ../. "Cargo.toml";
            vendoredCargoConfig = vendoredCargo + "/.cargo/config";
            wrappedCargo = pkgs.writeShellScriptBin "cargo" ''
               export CARGO_HOME="$out/cargo"
               exec ${pkgs.cargo}/bin/cargo "$@"
            '';
          in
          rec {
            src = ../.;
            workspace_member = "bin/runtime";
            buildInputs = [pkgs.git pkgs.cacert];
            CARGO = "${wrappedCargo}/bin/cargo";
            CARGO_HOME="$out/cargo";
            preConfigure = ''
              mkdir -p "$out/cargo"
              cp -r ${vendoredCargoConfig} $out/cargo/config
              ln -s ${vendoredCargo} $out/cargo-vendor-dir
              cp ${vendoredCargo}/Cargo.lock $out/Cargo.lock
            '';
            postBuild = ''
              rm -rf $out
            '';
          };
    }
    );
  };
  generated = generatedCargoNix {
    name = "aleph-node";
    src = ../.;
  };
  project = import generated { pkgs = nixpkgs; buildRustCrateForPkgs = customBuildRustCrateForPkgs; };
in
{ inherit project generated; }

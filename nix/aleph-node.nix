{ rocksDBVersion ? "6.29.3"
, versions ? import ./versions.nix
, nixpkgs ? versions.nixpkgs
, gitignoreSource ? versions.gitignoreSource
}:
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

  # allows to skip files listed by .gitignore
  # otherwise `nix-build` copies everything, including the target directory
  src = gitignoreSource ../.;

  crate2nix = nixpkgs.crate2nix;
  inherit (import ./tools.nix { pkgs = nixpkgs; lib = nixpkgs.lib; stdenv = env; inherit crate2nix; }) generatedCargoNix vendoredCargoLock;

  # some of our dependencies requires external libraries like protobuf, etc.
  customBuildRustCrateForPkgs = pkgs: pkgs.buildRustCrate.override {
    stdenv = env;
    defaultCrateOverrides = pkgs.defaultCrateOverrides // (
      let protobufFix = attrs: {
            # provides env variables necessary to use protobuf during compilation
            buildInputs = [ pkgs.protobuf ];
            PROTOC="${pkgs.protobuf}/bin/protoc";
          };
      in rec {
        librocksdb-sys = attrs: {
          buildInputs = [ customRocksdb ];
          ROCKSDB_LIB_DIR="${customRocksdb}/lib";
          # forces librocksdb-sys to statically compile with our customRocksdb
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
          # this is a bit tricky - aleph-runtime's build.rs calls Cargo, so we need to provide it a populated
          # CARGO_HOME, otherwise it tries to download crates  (it doesn't work with sandboxed nix-build)
          let
            vendoredCargo = vendoredCargoLock "${src}" "Cargo.toml";
            vendoredCargoConfig = vendoredCargo + "/.cargo/config";
            CARGO_HOME_BASE="$out/.cargo-home";
            CARGO_HOME="${CARGO_HOME_BASE}/cargo";
            # this way Cargo called by build.rs can see our vendored CARGO_HOME
            wrappedCargo = pkgs.writeShellScriptBin "cargo" ''
               export CARGO_HOME="$out/cargo"
               exec ${pkgs.cargo}/bin/cargo "$@"
            '';
          in
          rec {
            inherit src CARGO_HOME CARGO_HOME_BASE;
            # otherwise it has no access to other dependencies in our workspace
            workspace_member = "bin/runtime";
            buildInputs = [pkgs.git pkgs.cacert];
            CARGO = "${wrappedCargo}/bin/cargo";
            # build.rs is called during `configure` phase, so we need to setup during `preConfigure`
            preConfigure = ''
              # populates vendored CARGO_HOME
              mkdir -p $CARGO_HOME
              cp -r ${vendoredCargoConfig} $CARGO_HOME/config
              cp ${vendoredCargo}/Cargo.lock $CARGO_HOME_BASE/Cargo.lock
              ln -s ${vendoredCargo} $CARGO_HOME_BASE/cargo-vendor-dir
            '';
            postBuild = ''
              # we need to clean after ourselves
              # buildRustCrate derivation will populate it with necessary artifacts
              rm -rf $out
            '';
          };
    }
    );
  };

  generated = generatedCargoNix {
    name = "aleph-node";
    inherit src;
  };
  project = import generated { pkgs = nixpkgs; buildRustCrateForPkgs = customBuildRustCrateForPkgs; };
in
{ inherit project src; }

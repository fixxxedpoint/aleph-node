{
# defines whether target should be build in release or debug mode
  release ? true
# allows to strip binary from all debug info
, keepDebugInfo ? true
# name of this derivation
, name ? "aleph-node"
# attribute set of the form { "package_name" = [list_of_features] }
# defines which packages supposed to be build
, crates ? { "aleph-node" = []; }
# allows to run unit tests during the build procedure
, runTests ? false
# forces naersk (helper tool for building rust projects under nix) to build in a single derivation, instead default way that uses deps and project derivations
# it is used for building aleph-runtime (we don't want its dependencies to be build separately for a non-WASM architecture)
, singleStep ? false
# passed to rustc by cargo - it allows us to set the list of supported cpu features
# we can use for example `-C target-cpu=native` which should produce a binary that is significantly faster than the one produced using `generic`
# `generic` is the default `target-cpu` provided by cargo
, rustflags ? "-C target-cpu=generic"
# allows to build a custom version of rocksdb instead of using one build by librocksdb-sys
# our custom version includes couple of changes that should significantly speed it up
, useCustomRocksDb ? false
, setInterpreter ? { path = "/lib64/ld-linux-x86-64.so.2"; substitute = true; }
, cargoHomePath ? ""
, customBuildCommand ? ""
, versions ? import ./nix/versions.nix {}
}:
let
  providedCargoHome = cargoHomePath != "";
  cargoHome = builtins.path { path = builtins.toPath cargoHomePath; name = "cargo-home"; };

  nixpkgs = versions.nixpkgs;
  rustToolchain = versions.rustToolchain;

  # declares a build environment where C and C++ compilers are delivered by the llvm/clang project
  # in this version build process should rely only on clang, without access to gcc
  llvm = versions.llvm;
  env = if keepDebugInfo then nixpkgs.keepDebugInfo versions.stdenv else versions.stdenv;

  # tool for conveniently building rust projects
  naersk = versions.naersk.override { stdenv = env; };

  # WARNING this custom version of rocksdb is only build when useCustomRocksDb == true
  # we use a newer version of rocksdb than the one provided by nixpkgs
  # we disable all compression algorithms, force it to use SSE 4.2 cpu instruction set and disable its `verify_checksum` mechanism
  customRocksdb = versions.customRocksDB;

  # newer versions of Substrate support providing a version hash by means of an env variable, i.e. SUBSTRATE_CLI_GIT_COMMIT_HASH
  gitFolder = builtins.path { path = ./.git; name = "git-folder"; };
  gitCommit = builtins.readFile (
    nixpkgs.runCommand "gitCommit" { nativeBuildInputs = [nixpkgs.git]; } ''
      GIT_DIR=${gitFolder} git rev-parse --short HEAD > $out
    ''
  );

  modePath = if release then "release" else "debug";
  pathToWasm = "target/" + modePath + "/wbuild/aleph-runtime/target/wasm32-unknown-unknown/" + modePath + "/aleph_runtime.wasm";
  pathToCompactWasm = "target/" + modePath + "/wbuild/aleph-runtime/aleph_runtime.compact.wasm";

  featureIntoPrefixedFeature = packageName: feature: packageName + "/" + feature;
  featuresIntoPrefixedFeatures = package: features: builtins.map (featureIntoPrefixedFeature package) features;
  prefixedFeatureList = builtins.concatLists (nixpkgs.lib.mapAttrsToList featuresIntoPrefixedFeatures crates);

  enabledFeatures = nixpkgs.lib.concatStringsSep "," prefixedFeatureList;
  featuresFlag = if enabledFeatures == "" then "" else "--features " + enabledFeatures;
  packageFlags = if crates == {} then "" else builtins.map (crate: "--package " + crate) (builtins.attrNames crates);

  # allows to skip files listed by .gitignore
  # otherwise `nix-build` copies everything, including the target directory
  inherit (versions.gitignore) gitignoreFilter;
  # we need to include the .git directory, since Substrate's build scripts use git to retrieve hash of git's HEAD
  gitFilter = src:
    let
      srcIgnored = gitignoreFilter src;
    in
      path: type:
        builtins.baseNameOf path == ".git" || srcIgnored path type;
  src = nixpkgs.lib.cleanSourceWith {
    src = ./.;
    filter = gitFilter ./.;
    name = "aleph-source";
  };
  customBuild = old:
    if customBuildCommand != "" then customBuildCommand else old;
in
with nixpkgs; naersk.buildPackage rec {
  inherit src name release singleStep;
  nativeBuildInputs = [
    git
    pkg-config
    llvm.libclang
    protobuf
  ] ++ nixpkgs.lib.optional setInterpreter.substitute patchelf;
  propagatedBuildInputs = nixpkgs.lib.optional useCustomRocksDb customRocksdb;
  cargoBuild = customBuild;
  cargoBuildOptions = opts:
    packageFlags
    ++ [featuresFlag]
    ++
    [
      # require Cargo.lock is up to date
      "--locked"
      # run cargo without accessing the network
      "--offline"
    ]
    ++ opts;
  # provides necessary env variables
  shellHook = ''
    # this is the way we can pass additional arguments to rustc that is called by cargo, e.g. list of available cpu features
    export RUSTFLAGS="${rustflags}"

    # it allows us to provide hash of the git's HEAD, which is used as part of the version string returned by aleph-node
    # see https://github.com/paritytech/substrate/blob/5597a93a8c8b1ab578693c68549e3ce1902f3eaf/utils/build-script-utils/src/version.rs#L22
    export SUBSTRATE_CLI_GIT_COMMIT_HASH="${gitCommit}"

    # libp2p* rust libraries depends (indirectly) on protobuf
    # https://github.com/tokio-rs/prost/blob/7c0916d908c2d088ddb64a7e8849bfc839f6a3de/prost-build/build.rs#L30
    export PROTOC="${protobuf}/bin/protoc";

    # following two exports are required in order to build librocksdb-sys
    # some of the custom build.rs scripts of our dependencies use libclang while building their c/c++ depdendencies
    export LIBCLANG_PATH="${llvm.libclang.lib}/lib"
    # Set C flags for Rust's bindgen program. Unlike ordinary C
    # compilation, bindgen does not invoke $CC directly. Instead it
    # uses LLVM's libclang. To make sure all necessary flags are
    # included we need to look in a few places.
    # https://github.com/rust-lang/rust-bindgen/blob/89032649044d875983a851fff6fbde2d4e2ceaeb/src/lib.rs#L213
    export BINDGEN_EXTRA_CLANG_ARGS=$(cat ${env.cc}/nix-support/{cc,libc}-cflags)
  '';
  preConfigure = ''
    ${shellHook}
  '';
  postConfigure = ''
      ${nixpkgs.lib.optionalString providedCargoHome
         ''
           export CARGO_HOME=${cargoHome}
         ''
       }
  '';
  # called after successful build - copies aleph-runtime WASM binaries and sets appropriate interpreter (compatibility with other linux distros)
  postInstall = ''
    if [ -f ${pathToWasm} ]; then
      mkdir -p $out/lib
      cp ${pathToWasm} $out/lib/
    fi
    if [ -f ${pathToCompactWasm} ]; then
      mkdir -p $out/lib
      cp ${pathToCompactWasm} $out/lib/
    fi
    ${nixpkgs.lib.optionalString setInterpreter.substitute "if [[ -d $out/bin && $(find $out/bin/ -type f | wc -l) > 0 ]]; then find $out/bin -type f | xargs patchelf --set-interpreter ${setInterpreter.path}; fi"}
  '';

}

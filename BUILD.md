### Build
We provide a build procedure based on the `nix` package manager. There are several ways to interact with this process. Users can install `nix` locally or interact with by the means of docker. We prepared a simple docker-image that provides one with necessary tools for the build process. One can attempt at reproducing the build process without using `nix` by simply installing all dependencies described in the `shell.nix` file and following execution of its `buildPhase`.

In order to build a binary for `aleph-node` using docker one needs to install docker locally, i.e. in case of the Ubuntu linux distribution, by executing `sudo apt install docker.io`. Next step is to prepare our docker-image that handles the build process, by invoking:
```
docker build -t aleph-node/build -f docker/Dockerfile_build .
```
Created docker-image contains all necessary native build-time dependencies of `aleph-node` (without rust/cargo dependencies).
One can interact with such docker-image in two ways, using the `nix-shell` or `nix-build` command:
`nix-shell` - spawns a shell that includes all build dependencies:
```
docker run -ti --volume=$(pwd):/node/build aleph-node/build -s
cargo build --release -p aleph-node # it will build `aleph-node` in cargo's default target directory, i.e. `./target/x86_64-unknown-linux-gnu/release/aleph-node`
```
This way, our docker instance maintains all build artifacts inside of project's root directory, which allows to speed up ongoing build invocations, i.e. next time one invokes `cargo build` it should take significantly less time.

Another way to interact with this docker image is to allow it to only provide us with a single `aleph-node` binary artifact, i.e. each time we call its build process it will start it from scratch in a isolated environment.
```
docker run -ti --volume=$(pwd):/node/build aleph-node/build # outputs the `aleph-node` binary in current dir
```

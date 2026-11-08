# Installation

Barbacane ships two programs:

- `barbacane`: the gateway (the data plane) and the command-line tool, with `init`, `dev`, `validate`, `compile`, `serve`, `key` and `control`.
- `barbacane-control`: the control plane, an API that stores specs, compiles them and deploys artifacts to connected gateways. You only need it to manage gateways centrally; see the [control plane guide](control-plane.md).

Install them from the release binaries, the container images, crates.io, or the source.

## Release binaries

Every [GitHub release](https://github.com/barbacane-dev/barbacane/releases) carries both programs for:

| Platform | Target |
|----------|--------|
| Linux x86_64 (glibc) | `x86_64-unknown-linux-gnu` |
| Linux x86_64 (static, musl) | `x86_64-unknown-linux-musl` |
| Linux ARM64 (glibc) | `aarch64-unknown-linux-gnu` |
| Linux ARM64 (static, musl) | `aarch64-unknown-linux-musl` |
| macOS Intel | `x86_64-apple-darwin` |
| macOS Apple Silicon | `aarch64-apple-darwin` |

The assets are named `barbacane-<target>` and `barbacane-control-<target>`. Download the latest stable release, check it against the release's `checksums.txt`, and put it on your `PATH`:

```bash
TARGET=aarch64-apple-darwin   # one of the targets above
curl -LO https://github.com/barbacane-dev/barbacane/releases/latest/download/barbacane-$TARGET
curl -LO https://github.com/barbacane-dev/barbacane/releases/latest/download/checksums.txt
shasum -a 256 --ignore-missing -c checksums.txt   # on Linux: sha256sum --ignore-missing -c checksums.txt
chmod +x barbacane-$TARGET
sudo mv barbacane-$TARGET /usr/local/bin/barbacane
barbacane --version
```

`latest` is the last stable release. A release candidate, or any other version, is downloaded by its tag:

```bash
curl -LO https://github.com/barbacane-dev/barbacane/releases/download/v1.0.0-rc.1/barbacane-$TARGET
curl -LO https://github.com/barbacane-dev/barbacane/releases/download/v1.0.0-rc.1/checksums.txt
```

The same release carries the official plugins (`<name>.wasm`, with `plugin-checksums.txt`). `barbacane init --fetch-plugins` downloads the ones a new project uses from the release of its own version.

## Container images

Each release publishes three images, to the GitHub Container Registry and to Docker Hub, for `linux/amd64` and `linux/arm64`:

| Image | Contents |
|-------|----------|
| `ghcr.io/barbacane-dev/barbacane`, `barbacane/barbacane` | The gateway (`barbacane`) |
| `ghcr.io/barbacane-dev/barbacane-control`, `barbacane/barbacane-control` | The control plane, serving its API on port 9090 |
| `ghcr.io/barbacane-dev/barbacane-standalone`, `barbacane/barbacane-standalone` | The gateway with every official plugin, to compile and serve without the repository |

Tags:

- `1.2.3`: that release;
- `latest`, `1.2` and `1`: the last stable release, of all, of that minor version, and of that major version;
- a release candidate such as `1.0.0-rc.1` has its own tag only, so `latest`, `1.2` and `1` never move to a pre-release.

```bash
docker pull ghcr.io/barbacane-dev/barbacane:latest
docker pull barbacane/barbacane-control:1.0.0-rc.1
```

The gateway serves only artifacts signed by the key it pins. Pass that key's public key (see [the getting started guide](getting-started.md#run-it-in-production)):

```bash
docker run --rm -p 8080:8080 -v ./api.bca:/config/api.bca \
  -e BARBACANE_TRUSTED_PUBKEY=<public key> \
  ghcr.io/barbacane-dev/barbacane serve --artifact /config/api.bca --listen 0.0.0.0:8080
```

## crates.io

With a Rust toolchain (the minimum supported version is 1.95):

```bash
cargo install barbacane
cargo install barbacane-control   # the control plane, if you run one
```

## From source

```bash
git clone https://github.com/barbacane-dev/barbacane.git
cd barbacane
cargo build --release -p barbacane -p barbacane-control
# target/release/barbacane and target/release/barbacane-control

make plugins   # the official plugins, as plugins/<name>/<name>.wasm
```

Building the plugins needs the `wasm32-unknown-unknown` target: `rustup target add wasm32-unknown-unknown`.

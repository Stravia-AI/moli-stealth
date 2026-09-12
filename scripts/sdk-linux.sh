#!/bin/sh
# Invoked inside a native Debian 12 or Alpine 3.23 container by sdk.yml.
set -eu
mode=$1
target=$2
export DEBIAN_FRONTEND=noninteractive
if [ -f /etc/alpine-release ]; then
    case "$(cat /etc/alpine-release)" in 3.23.*) ;; *) echo 'Expected Alpine 3.23' >&2; exit 1 ;; esac
    apk add --no-cache build-base ca-certificates curl git python3 llvm clang
    if [ "$mode" = build ]; then
        apk add --no-cache cmake clang-dev pkgconf fontconfig-dev fontconfig-static \
            freetype-static expat-static bzip2-static brotli-static libpng-static zlib-static \
            fontconfig-doc freetype-doc expat-doc bzip2-doc brotli-doc libpng-doc zlib-doc gcc-doc
    else
        apk add --no-cache fontconfig font-dejavu font-noto-cjk libgcc
    fi
else
    . /etc/os-release
    test "$ID" = debian && test "$VERSION_ID" = 12
    apt-get update
    apt-get install --yes --no-install-recommends build-essential ca-certificates curl git python3 llvm clang
    if [ "$mode" = build ]; then
        apt-get install --yes --no-install-recommends cmake libclang-dev pkg-config \
            libfontconfig-dev libfreetype-dev libexpat1-dev libbz2-dev libbrotli-dev libpng-dev zlib1g-dev
    else
        apt-get install --yes --no-install-recommends fontconfig fonts-dejavu-core fonts-noto-cjk libgcc-s1
    fi
fi
# No glibc compatibility package, Zig sysroot, or transplanted build font paths.
export PATH="/opt/go/bin:/root/.cargo/bin:$PATH"
curl --proto '=https' --tlsv1.2 --fail --silent --show-error https://sh.rustup.rs -o /tmp/rustup-init.sh
sh /tmp/rustup-init.sh -y --profile minimal --default-host "$target" --default-toolchain "$(cat /src/rust-toolchain)-$target"
git config --global --add safe.directory /src
cd /src
if [ "$mode" = build ]; then
    go version
    pkg-config --static --libs fontconfig
    if [ -f /etc/alpine-release ]; then
        chmod +x scripts/sdk-host-rustc.py
        export RUSTC_WRAPPER=/src/scripts/sdk-host-rustc.py
    fi
    # The SDK uses one static C++ runtime across V8 and the other native
    # dependencies. Archive, bindings and notices are one pinned variant.
    export RUSTY_V8_MOLI_LIBSTDCXX=1 CXXSTDLIB=''
    case "$target" in
        x86_64-unknown-linux-gnu)
            export RUSTY_V8_ARCHIVE_SHA256=b7ec33e5a75a1f11fb986ec329a4dd0e170c578f2dd54f836d87f1d918f9e712 ;;
        aarch64-unknown-linux-gnu)
            export RUSTY_V8_ARCHIVE_SHA256=76c200a647bd5fdbde1eb336e469506b66eb2bcb5ad91ac00b4b9c061b99a7c5 ;;
        x86_64-unknown-linux-musl)
            export RUSTY_V8_ARCHIVE_SHA256=eee8861d551df976c2da013ffc087431b64009b174de8ba066fc4a38c7fe8204 ;;
        aarch64-unknown-linux-musl)
            export RUSTY_V8_ARCHIVE_SHA256=097e4b6b8603a32ed900e44e5d071fa30783840823b412c64dfdeb6b6d831c4b ;;
        *)
            echo "Unsupported Linux SDK target: $target" >&2
            exit 1 ;;
    esac
    v8_release=https://github.com/Stravia-AI/rusty_v8/releases/download/v152.2.0-moli-sdk-libstdcxx.1
    export RUSTY_V8_ARCHIVE="$v8_release/librusty_v8_moli_libstdcxx_release_${target}.a.gz"
    mkdir -p /tmp/moli-v8/notices
    export RUSTY_V8_SRC_BINDING_PATH="/tmp/moli-v8/src_binding_moli_libstdcxx_release_${target}.rs"
    curl --proto '=https' --tlsv1.2 --fail --location --silent --show-error \
        "$v8_release/src_binding_moli_libstdcxx_release_${target}.rs" -o "$RUSTY_V8_SRC_BINDING_PATH"
    printf '%s  %s\n' 9dbb21c67bf9c97424f8d65e7e49c4a270f495389cd0bb26092439b0ad8a5173 "$RUSTY_V8_SRC_BINDING_PATH" | sha256sum -c
    curl --proto '=https' --tlsv1.2 --fail --location --silent --show-error \
        "$v8_release/moli-v8-native-notices-${target}.tar.gz" -o /tmp/moli-v8/notices.tar.gz
    printf '%s  %s\n' a102daeadeeb68526682be8061da193a7dae6145cdae95f42e524fda62bcf988 /tmp/moli-v8/notices.tar.gz | sha256sum -c
    tar -xzf /tmp/moli-v8/notices.tar.gz -C /tmp/moli-v8/notices
    export RUSTFLAGS="${RUSTFLAGS:-} -L native=$(dirname "$(g++ -print-file-name=libstdc++.a)") -L native=$(dirname "$(g++ -print-file-name=libgcc_eh.a)") -L native=$(dirname "$(g++ -print-file-name=libatomic.a)")"
    python3 scripts/sdk-package.py --target "$target" --native-notices /usr/share/doc --native-notices /tmp/moli-v8/notices
elif [ "$mode" = verify ]; then
    # The bound commit contains only the final digest binding on top of implementation.
    git fetch /src/bound/sdk-bound.bundle refs/heads/sdk-bound:refs/heads/sdk-bound
    git checkout sdk-bound
    revision=$(git rev-parse HEAD)
    export FONTCONFIG_FILE=/etc/fonts/fonts.conf
    export FONTCONFIG_PATH=/etc/fonts
    fc-cache -f
    mkdir -p /src/dist/evidence
    trap 'for file in /tmp/sdk-consumer/*.log /tmp/sdk-consumer/*.json /tmp/sdk-consumer/*.jsonl; do if [ -f "$file" ]; then cp "$file" /src/dist/evidence/; fi; done; if [ -d /tmp/sdk-consumer/evidence ]; then cp -R /tmp/sdk-consumer/evidence /src/dist/evidence/rendering; fi' EXIT
    if rustup toolchain install "1.98.1-$target" --profile minimal --no-self-update > /src/dist/evidence/cross-rust-1.98.1-install.log 2>&1; then
        cat /src/dist/evidence/cross-rust-1.98.1-install.log
    else
        cat /src/dist/evidence/cross-rust-1.98.1-install.log >&2
        echo "Required native Rust 1.98.1 toolchain unavailable: $target" >&2
        exit 1
    fi
    fc-match 'DejaVu Sans' > /src/dist/evidence/font-match.txt
    fc-match 'Noto Sans CJK SC' >> /src/dist/evidence/font-match.txt
    python3 scripts/sdk-bind.py seed --target "$target" --artifacts /src/artifacts --cache /tmp/sdk-cache
    python3 sdk-consumer/verify.py --sdk-revision "$revision" --repository file:///src \
        --destination /tmp/sdk-consumer --target "$target" --cache-dir /tmp/sdk-cache
    python3 scripts/sdk-audit.py --target "$target" --consumer /tmp/sdk-consumer --output /src/dist/evidence/runtime.json
    python3 scripts/sdk-audit.py --target "$target" --consumer /tmp/sdk-consumer/cross-rust-1.98.1 --profile debug --output /src/dist/evidence/runtime-rust-1.98.1.json
    python3 sdk-consumer/font-check.py --consumer /tmp/sdk-consumer --target "$target"
else
    echo "Unknown SDK container mode: $mode" >&2
    exit 1
fi

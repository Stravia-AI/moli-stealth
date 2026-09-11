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
sh /tmp/rustup-init.sh -y --profile minimal --default-toolchain "$(cat /src/rust-toolchain)"
git config --global --add safe.directory /src
cd /src
if [ "$mode" = build ]; then
    go version
    pkg-config --static --libs fontconfig
    # Real v152.2.0 upstream release assets; never substitute GNU objects.
    # Digests are pinned from the published GitHub release metadata, not fetched at build time.
    case "$target" in
        x86_64-unknown-linux-musl)
            export RUSTY_V8_ARCHIVE_SHA256=68f284fb8184b9f9e8d7e23b8f2675dbe464f43f77c0bf152f705bf7ead29558 ;;
        aarch64-unknown-linux-musl)
            export RUSTY_V8_ARCHIVE_SHA256=419a242557833151cf9dde82c12c8272c3c00015bfe7e340e48164a600d34039 ;;
    esac
    case "$target" in
        *-musl)
            export RUSTY_V8_ARCHIVE="https://github.com/denoland/rusty_v8/releases/download/v152.2.0/librusty_v8_release_${target}.a.gz"
            export RUSTY_V8_SRC_BINDING_URL="https://github.com/denoland/rusty_v8/releases/download/v152.2.0/src_binding_release_${target}.rs" ;;
    esac
    python3 scripts/sdk-package.py --target "$target" --native-notices /usr/share/doc
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
    fc-match 'DejaVu Sans' > /src/dist/evidence/font-match.txt
    fc-match 'Noto Sans CJK SC' >> /src/dist/evidence/font-match.txt
    python3 scripts/sdk-bind.py seed --target "$target" --artifacts /src/artifacts --cache /tmp/sdk-cache
    python3 sdk-consumer/verify.py --sdk-revision "$revision" --repository file:///src \
        --destination /tmp/sdk-consumer --target "$target" --cache-dir /tmp/sdk-cache
    python3 scripts/sdk-audit.py --target "$target" --consumer /tmp/sdk-consumer --output /src/dist/evidence/runtime.json
    python3 sdk-consumer/font-check.py --consumer /tmp/sdk-consumer --target "$target"
else
    echo "Unknown SDK container mode: $mode" >&2
    exit 1
fi

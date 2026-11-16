#!/bin/sh
# Module: mirante.build
# Purpose: Builds, tests and packages Mirante as a .deb for Ubuntu, Debian and Mint.
# Created: 2026-11-16
# Architecture: Build on the oldest supported release (Ubuntu 24.04) so the
# binaries' glibc requirement suits all targets. Needs rustup's stable toolchain,
# libgtk-3-dev and dpkg-deb. Outputs dist/mirante_<version>_<arch>.deb, one
# dist/plugins/<id>.mirante-plugin bundle per format, and target/plugins for
# running from the source tree: MIRANTE_PLUGIN_PATH=target/plugins target/release/mirante
set -eu
cd "$(dirname "$0")"
export PATH="$HOME/.cargo/bin:$PATH"
FORMATS="bmp heic jpeg png psd raw tiff"

cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --release --workspace

ADMIN=target/release/mirante-plugin-admin
rm -rf target/plugins dist/plugins
mkdir -p target/plugins dist/plugins
for format in $FORMATS; do
    executable="mirante-plugin-$format"
    mkdir -p "target/plugins/$format"
    install -m 755 "target/release/$executable" "target/plugins/$format/"
    "target/release/$executable" manifest > "target/plugins/$format/plugin.ini"
    chmod 644 "target/plugins/$format/plugin.ini"
    "$ADMIN" pack "target/plugins/$format" "dist/plugins/$format.mirante-plugin"
done

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)
ARCH=$(dpkg --print-architecture)
ROOT=target/deb/mirante
rm -rf "$ROOT"
mkdir -p "$ROOT/DEBIAN" "$ROOT/usr/bin" "$ROOT/usr/lib/mirante" \
    "$ROOT/usr/share/applications" "$ROOT/usr/share/icons/hicolor/scalable/apps" \
    "$ROOT/usr/share/polkit-1/actions" "$ROOT/usr/share/doc/mirante"

install -m 755 target/release/mirante "$ROOT/usr/bin/mirante"
install -m 755 "$ADMIN" "$ROOT/usr/lib/mirante/"
cp -r target/plugins "$ROOT/usr/lib/mirante/plugins"
install -m 644 packaging/icons/io.github.mirante.Viewer.svg "$ROOT/usr/share/icons/hicolor/scalable/apps/"
install -m 644 packaging/io.github.mirante.plugins.policy "$ROOT/usr/share/polkit-1/actions/"
install -m 644 assets/flags/COPYING.flag-icons "$ROOT/usr/share/doc/mirante/"
MIME_TYPES=$(sed -n 's/^mime-types=//p' target/plugins/*/plugin.ini | tr ';' '\n' | sort -u | sed 's/$/;/' | tr -d '\n')
sed "s|@MIME_TYPES@|$MIME_TYPES|" packaging/io.github.mirante.Viewer.desktop.in \
    > "$ROOT/usr/share/applications/io.github.mirante.Viewer.desktop"
chmod 644 "$ROOT/usr/share/applications/io.github.mirante.Viewer.desktop"
sed -e "s/@VERSION@/$VERSION/" -e "s/@ARCH@/$ARCH/" packaging/debian/control.in > "$ROOT/DEBIAN/control"
install -m 755 packaging/debian/postinst packaging/debian/prerm packaging/debian/postrm "$ROOT/DEBIAN/"

dpkg-deb --root-owner-group --build "$ROOT" "dist/mirante_${VERSION}_${ARCH}.deb"

#!/usr/bin/env bash
# Install/update rust.
# The first argument should be the toolchain to install.

set -ex

if [ -z "$TOOLCHAIN" ]
then
    echo "TOOLCHAIN must  be set."
    exit 1
fi

if [ -z "$TARGET" ]
then
    echo "TARGET must  be set."
    exit 1
fi

# case $OS in
#   ubuntu*)
#     # Install OpenSSLs
#     sudo apt-get install -y pkg-config libssl-dev
#     ;;
#   *)
#     echo "Skip OpenSSL installation"
#     ;;
# esac

rustup set profile minimal
rustup component remove --toolchain=$TOOLCHAIN rust-docs || echo "already removed"
rustup update --no-self-update $TOOLCHAIN

HOST=$(rustc -Vv | grep ^host: | sed -e "s/host: //g")
if [ "$HOST" != "$TARGET" ]
then
    rustup component add llvm-tools-preview --toolchain=$TOOLCHAIN
    rustup component add rust-std-$TARGET --toolchain=$TOOLCHAIN
fi

rustup default $TOOLCHAIN
rustup -V
rustc -Vv
cargo -V

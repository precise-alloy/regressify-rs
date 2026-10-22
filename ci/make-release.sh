#!/usr/bin/env bash
# Builds the release and creates archives with SHA-256 files. Publication is a separate
# step (ci/release_feed.py stage and ci/deploy-cloudflare.sh).
set -ex

if [ -z "$FILE_NAME" ]
then
  echo "FILE_NAME must be set"
  exit 1
fi

if [ -z "$GITHUB_REF" ]
then
  echo "GITHUB_REF must be set"
  exit 1
fi

if [ -z "$OS" ]
then
  echo "OS must be set"
  exit 1
fi

if [ -z "$PACKAGE_NAME" ]
then
  echo "PACKAGE_NAME must be set"
  exit 1
fi


if [ -z "$TARGET" ]
then
  echo "TARGET must be set"
  exit 1
fi


# Strip $FILE_NAME-refs/tags/ from the start of the ref.
TAG=${GITHUB_REF#*/tags/}

host=$(rustc -Vv | grep ^host: | sed -e "s/host: //g")
if [ "$host" != $TARGET ]
then
  export "CARGO_TARGET_$(echo $TARGET | tr a-z- A-Z_)_LINKER"=rust-lld
fi

export CARGO_PROFILE_RELEASE_LTO=true

# Embed the source commit hash into the binary so `regressify --version` /
# `regressify version` report exactly which build is running. Prefer an
# explicitly provided hash, then the CI-provided GITHUB_SHA, then git.
if [ -z "$BINARY_GIT_HASH" ]
then
  if [ -n "$GITHUB_SHA" ]
  then
    BINARY_GIT_HASH="$GITHUB_SHA"
  elif command -v git &> /dev/null && git rev-parse HEAD &> /dev/null
  then
    BINARY_GIT_HASH="$(git rev-parse HEAD)"
  else
    BINARY_GIT_HASH="unknown"
  fi
fi

export BINARY_GIT_HASH
echo "Embedding git hash: $BINARY_GIT_HASH"

if [ -z "$SKIP_BUILD" ]
then
  build_args=(--locked --bin "$FILE_NAME" --release --target "$TARGET")
  if [ -n "$NO_DEFAULT_FEATURES" ]
  then
    build_args+=(--no-default-features)
  fi
  if [ -n "$CARGO_FEATURES" ]
  then
    build_args+=(--features "$CARGO_FEATURES")
  fi
  cargo build "${build_args[@]}"
fi

SOURCE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

cd "${CARGO_TARGET_DIR:-target}/$TARGET/release"
case $OS in
  ubuntu*)
    # asset="$PACKAGE_NAME-$TAG-$TARGET.tar.gz"
    asset=$FILE_NAME
    ;;
  macos*)
    # asset="$PACKAGE_NAME-$TAG-$TARGET.tar.gz"
    asset=$FILE_NAME
    ;;
  windows*)
    # asset="$PACKAGE_NAME-$TAG-$TARGET.zip"
    asset="$FILE_NAME.exe"
    ;;
  *)
    echo "OS must be set"
    exit 1
    ;;
esac

# Try to compress with UPX if available (skip for installer payloads so PE
# version resources, static-runtime imports, and payload hashes stay intact).
if [ -z "$SKIP_BUILD" ] && command -v upx &> /dev/null
then
  echo "Compressing binary with UPX..."
  upx --best --lzma "$asset"
else
  echo "UPX not found or skipped."
fi

PACKAGE_7Z="$PACKAGE_NAME-$TARGET.7z"
PACKAGE_ZIP="$PACKAGE_NAME-$TARGET.zip"
PACKAGE_7Z_SHA256="$PACKAGE_7Z.sha256"
PACKAGE_ZIP_SHA256="$PACKAGE_ZIP.sha256"

# If package is already exists, remove it
if [ -f $PACKAGE_7Z ]
then
  rm $PACKAGE_7Z
fi

if [ -f $PACKAGE_ZIP ]
then
  rm $PACKAGE_ZIP
fi

if [ -f $PACKAGE_7Z_SHA256 ]
then
  rm $PACKAGE_7Z_SHA256
fi

if [ -f $PACKAGE_ZIP_SHA256 ]
then
  rm $PACKAGE_ZIP_SHA256
fi

# AGPL recipients of the binary must also receive the license.
LICENSE_FILES=(LICENSE LICENSING.md)
for license_file in "${LICENSE_FILES[@]}"
do
  cp "$SOURCE_DIR/$license_file" .
done

if [ -n "$PASSWORD" ]
then
  # Password is set, enable header compression and encryption
  # -mhc: header compression
  # -mhe: header encryption
  # -mx[N] : set compression level: -mx1 (fastest) ... -mx9 (ultra)
  7z a $PACKAGE_7Z $asset "${LICENSE_FILES[@]}" -mhc=on -mhe=on -mx9 -p"$PASSWORD"

  # Also create a zip file
  7z a $PACKAGE_ZIP $asset "${LICENSE_FILES[@]}" -mx5 -p"$PASSWORD"
else
  # No password set, just compress
  7z a $PACKAGE_7Z $asset "${LICENSE_FILES[@]}" -mx9

  # Also create a zip file
  7z a $PACKAGE_ZIP $asset "${LICENSE_FILES[@]}" -mx5
fi

create_sha256_file() {
  file=$1

  if command -v sha256sum &> /dev/null
  then
    sha256sum "$file" > "$file.sha256"
  elif command -v shasum &> /dev/null
  then
    shasum -a 256 "$file" > "$file.sha256"
  else
    echo "sha256sum or shasum must be available"
    exit 1
  fi
}

create_sha256_file "$PACKAGE_7Z"
create_sha256_file "$PACKAGE_ZIP"

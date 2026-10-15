#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -ne 1 ] || [[ "$1" != /* ]]; then
  echo "Usage: build_ffmpeg_lgpl_windows_x64.sh /absolute/output/media" >&2
  exit 2
fi

media_dir="$1"
source_dir="${FILEID_MEDIA_SOURCE_DIR:-}"
temporary_source=0
if [ -z "$source_dir" ]; then
  source_dir="$(mktemp -d)"
  temporary_source=1
fi
build_dir=""
cleanup() {
  if [ -n "$build_dir" ]; then rm -rf "$build_dir"; fi
  if [ "$temporary_source" -eq 1 ]; then rm -rf "$source_dir"; fi
}
trap cleanup EXIT
if [[ "$source_dir" != /* ]]; then
  echo "FILEID_MEDIA_SOURCE_DIR must be absolute" >&2
  exit 2
fi
mkdir -p "$source_dir" "$media_dir"
cd "$source_dir"

ffmpeg_version=9.0.2
ffmpeg_sha=8c3850283eb25fa026482078a04051e0be17347b09ef81a0849bec15a96e002e
openh264_commit=652bdb7719f30b52b08e506645a7322ff1b2cc6f
ffmpeg_tar="ffmpeg-$ffmpeg_version.tar.xz"
if [ ! -f "$ffmpeg_tar" ]; then
  curl --fail --location --retry 3 --output "$ffmpeg_tar" "https://ffmpeg.org/releases/$ffmpeg_tar"
fi
if [ ! -f "$ffmpeg_tar.asc" ]; then
  curl --fail --location --retry 3 --output "$ffmpeg_tar.asc" "https://ffmpeg.org/releases/$ffmpeg_tar.asc"
fi
if [ ! -f ffmpeg-devel.asc ]; then
  curl --fail --location --retry 3 --output ffmpeg-devel.asc https://ffmpeg.org/ffmpeg-devel.asc
fi
printf '%s  %s\n' "$ffmpeg_sha" "$ffmpeg_tar" | sha256sum --check --status
mkdir -p gnupg
chmod 700 gnupg
GNUPGHOME="$source_dir/gnupg"
export GNUPGHOME
gpg --batch --import ffmpeg-devel.asc >/dev/null 2>&1
fingerprint="$(gpg --batch --with-colons --fingerprint ffmpeg-devel@ffmpeg.org | awk -F: '$1 == "fpr" { print $10; exit }')"
if [ "$fingerprint" != FCF986EA15E6E293A5644F10B4322F04D67658D8 ]; then
  echo "FFmpeg signing key fingerprint mismatch" >&2
  exit 1
fi
gpg --batch --verify "$ffmpeg_tar.asc" "$ffmpeg_tar" >/dev/null 2>&1

if [ ! -d openh264/.git ]; then
  git clone --depth 1 --branch v2.6.0 https://github.com/cisco/openh264.git openh264
fi
if [ "$(git -C openh264 rev-parse HEAD)" != "$openh264_commit" ]; then
  echo "OpenH264 source commit mismatch" >&2
  exit 1
fi
build_dir="$(mktemp -d "$source_dir/fileid-media-build.XXXXXX")"
tar -xf "$ffmpeg_tar" -C "$build_dir"
git -C openh264 archive --format=tar.gz --prefix=openh264/ HEAD > "$build_dir/openh264-source.tar.gz"
tar -xf "$build_dir/openh264-source.tar.gz" -C "$build_dir"

prefix="$build_dir/prefix"
make -C "$build_dir/openh264" -j2 OS=mingw_nt ARCH=x86_64 \
  CC=x86_64-w64-mingw32-gcc CXX=x86_64-w64-mingw32-g++ \
  AR=x86_64-w64-mingw32-ar RANLIB=x86_64-w64-mingw32-ranlib \
  STRIP=x86_64-w64-mingw32-strip USE_ASM=No \
  LDFLAGS='-static-libgcc -static-libstdc++' PREFIX="$prefix" install-shared
export PKG_CONFIG_PATH="$prefix/lib/pkgconfig"
cd "$build_dir/ffmpeg-$ffmpeg_version"
./configure \
  --prefix="$prefix" --target-os=mingw32 --arch=x86_64 \
  --cross-prefix=x86_64-w64-mingw32- --pkg-config=pkg-config \
  --disable-gpl --disable-nonfree --disable-network --disable-autodetect \
  --disable-x86asm --disable-doc --disable-ffplay \
  --disable-static --enable-shared --enable-libopenh264 \
  --extra-ldflags=-static-libgcc
make -j2
make install

cp -a "$prefix/bin/ffmpeg.exe" "$prefix/bin/ffprobe.exe" "$prefix"/bin/*.dll "$media_dir/"
cp "$source_dir/$ffmpeg_tar" "$source_dir/$ffmpeg_tar.asc" "$media_dir/"
git -C "$source_dir/openh264" archive --format=tar.gz --prefix=openh264-2.6.0/ HEAD > "$media_dir/openh264-2.6.0.tar.gz"
cp "$build_dir/ffmpeg-$ffmpeg_version/COPYING.LGPLv2.1" "$media_dir/FFmpeg-LGPL-2.1.txt"
cp "$build_dir/openh264/LICENSE" "$media_dir/OpenH264-BSD-2-Clause.txt"
cat > "$media_dir/BUILD-AND-SOURCE.txt" <<'EOF'
FFmpeg 9.0.2 source: ffmpeg-9.0.2.tar.xz, signed by FFmpeg's release key.
OpenH264 2.6.0 source: openh264-2.6.0.tar.gz from commit 652bdb7719f30b52b08e506645a7322ff1b2cc6f.
Built for Windows x64 with this repository's shared/scripts/build_ffmpeg_lgpl_windows_x64.sh.
The build disables GPL, nonfree and network features, uses shared libraries, and enables OpenH264.
FileID invokes FFmpeg only for local media selected by the user.
This bundle does not establish codec patent clearance for distribution.
EOF
(
  cd "$media_dir"
  sha256sum ffmpeg.exe ffprobe.exe ./*.dll ffmpeg-9.0.2.tar.xz openh264-2.6.0.tar.gz > SHA256SUMS
)
for binary in "$media_dir"/*.exe "$media_dir"/*.dll; do
  imports="$(x86_64-w64-mingw32-objdump -p "$binary")"
  while read -r imported; do
    case "${imported,,}" in
      kernel32.dll|msvcrt.dll|shell32.dll|gdi32.dll|ole32.dll|oleaut32.dll|shlwapi.dll|user32.dll|avicap32.dll|bcrypt.dll|avcodec-63.dll|avdevice-63.dll|avfilter-12.dll|avformat-63.dll|avutil-61.dll|swresample-7.dll|swscale-10.dll|libopenh264.dll) ;;
      *) echo "Unexpected runtime dependency $imported in $binary" >&2; exit 1 ;;
    esac
  done < <(sed -n 's/^[[:space:]]*DLL Name: //p' <<< "$imports")
done
echo "Staged vetted Windows x64 development runtime in $media_dir"

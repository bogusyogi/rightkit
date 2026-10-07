#!/bin/bash
# Reproducible LGPL FFmpeg/FFprobe builds for the rightkit-media artifact catalog (macOS).
#
#   build-ffmpeg-macos.sh fetch --variant <variant> --sources <dir>
#       Download every source the variant needs into <dir> and print its sha256.
#       Never pins anything: copy observed digests into recipes/ffmpeg-sources.json
#       after comparing them with the upstream release announcement.
#
#   build-ffmpeg-macos.sh build --variant <variant> --arch arm64|x86_64 --tool ffmpeg|ffprobe \
#       --sources <dir> --work <dir> --out <dir>
#       Verify pinned source digests, build, and write <out>/<tool>-<variant>-macos-<arch>
#       plus <same>.evidence.json (sha256, -version, otool -L, configure line) and the
#       upstream notice files the catalog entry lists.
#
# Variants
#   decode           The approved catalog build (BUILD-NOTES-macos-*.txt): native codecs only.
#   export-lame-ass  decode + libmp3lame (MP3 export) + libass (subtitle burn-in) +
#                    zlib (PNG) + VideoToolbox/AudioToolbox (h264_videotoolbox).
#
# Both variants keep --disable-gpl --disable-nonfree --disable-autodetect. Third-party
# libraries are built static from pinned sources into a private prefix, and pkg-config is
# confined to that prefix (PKG_CONFIG_LIBDIR) so Homebrew or other host libraries can never
# be linked by accident. The finished binary may import only /usr/lib and /System libraries.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
manifest="$here/ffmpeg-sources.json"
cmd="${1:-}"
shift || true
variant="" arch="" tool="" sources="" work="" out=""
while [ $# -gt 0 ]; do
  case "$1" in
    --variant) variant="$2"; shift 2 ;;
    --arch) arch="$2"; shift 2 ;;
    --tool) tool="$2"; shift 2 ;;
    --sources) sources="$2"; shift 2 ;;
    --work) work="$2"; shift 2 ;;
    --out) out="$2"; shift 2 ;;
    *) echo "unknown argument $1" >&2; exit 2 ;;
  esac
done
case "$variant" in decode|export-lame-ass) ;; *) echo "--variant decode|export-lame-ass" >&2; exit 2 ;; esac
test -n "$sources" || { echo "--sources <dir> is required" >&2; exit 2; }

field() { python3 -c 'import json,sys; v=json.load(open(sys.argv[1]))["sources"][sys.argv[2]].get(sys.argv[3]); print("" if v is None else v)' "$manifest" "$1" "$2"; }
names() { python3 -c 'import json,sys; [print(k) for k,v in json.load(open(sys.argv[1]))["sources"].items() if sys.argv[2] in v["variants"]]' "$manifest" "$variant"; }
sha() { shasum -a 256 "$1" | cut -d' ' -f1; }

if [ "$cmd" = fetch ]; then
  mkdir -p "$sources"
  for name in $(names); do
    file="$sources/$(field "$name" file)"
    test -f "$file" || curl -fL --proto '=https' -o "$file" "$(field "$name" url)"
    pinned="$(field "$name" sha256)"
    got="$(sha "$file")"
    if [ -n "$pinned" ] && [ "$pinned" != "$got" ]; then
      echo "MISMATCH $name: pinned $pinned observed $got" >&2; exit 1
    fi
    echo "$name $(field "$name" version) sha256=$got ${pinned:+(pinned)}${pinned:-(UNPINNED: compare with upstream, then pin)}"
  done
  exit 0
fi

test "$cmd" = build || { echo "usage: $0 fetch|build ..." >&2; exit 2; }
case "$arch" in arm64) family=aarch64 ;; x86_64) family=x86_64 ;; *) echo "--arch arm64|x86_64" >&2; exit 2 ;; esac
case "$tool" in ffmpeg|ffprobe) ;; *) echo "--tool ffmpeg|ffprobe" >&2; exit 2 ;; esac
test -n "$work" && test -n "$out" || { echo "--work and --out are required" >&2; exit 2; }
test "$(uname -s)" = Darwin || { echo "macOS host required" >&2; exit 1; }

for name in $(names); do
  file="$sources/$(field "$name" file)"
  pinned="$(field "$name" sha256)"
  test -n "$pinned" || { echo "$name has no pinned sha256 in $manifest; run fetch and pin it first" >&2; exit 1; }
  test -f "$file" || { echo "missing source $file; run fetch first" >&2; exit 1; }
  test "$(sha "$file")" = "$pinned" || { echo "$name sha256 mismatch" >&2; exit 1; }
done

jobs="$(sysctl -n hw.ncpu)"
cc="clang -arch $arch"
host="$family-apple-darwin"
build_tag="$tool-$variant-macos-$arch"
root="$work/$build_tag"
prefix="$root/prefix"
rm -rf "$root"
mkdir -p "$root/src" "$prefix" "$out"
unpack() { tar -xf "$sources/$(field "$1" file)" -C "$root/src"; echo "$root/src/$1-$(field "$1" version)"; }

ffmpeg_flags=(
  --disable-gpl --disable-nonfree --disable-autodetect
  --enable-static --disable-shared --disable-doc --disable-debug --disable-ffplay --enable-small
  --arch="$arch" --cc="$cc"
)
if [ "$arch" = x86_64 ]; then
  ffmpeg_flags+=(--enable-cross-compile --target-os=darwin)
fi

notices=()
if [ "$variant" = export-lame-ass ]; then
  command -v meson >/dev/null || { echo "meson is required for harfbuzz" >&2; exit 1; }
  command -v ninja >/dev/null || { echo "ninja is required for harfbuzz" >&2; exit 1; }
  export PKG_CONFIG_LIBDIR="$prefix/lib/pkgconfig"
  unset PKG_CONFIG_PATH
  export CC="$cc" CFLAGS="-O2" CXX="clang++ -arch $arch"
  autotools=(--host="$host" --prefix="$prefix" --disable-shared --enable-static)

  src="$(unpack lame)"
  (cd "$src" && ./configure "${autotools[@]}" --disable-frontend --disable-decoder && make -j"$jobs" && make install)
  notices+=("lame:$src/$(field lame noticeFile)")

  src="$(unpack freetype)"
  (cd "$src" && ./configure "${autotools[@]}" --without-zlib --without-bzip2 --without-png \
      --without-harfbuzz --without-brotli && make -j"$jobs" && make install)
  notices+=("freetype:$src/$(field freetype noticeFile)")

  src="$(unpack fribidi)"
  (cd "$src" && ./configure "${autotools[@]}" --disable-docs && make -j"$jobs" && make install)
  notices+=("fribidi:$src/$(field fribidi noticeFile)")

  src="$(unpack harfbuzz)"
  cat > "$root/meson-$arch.ini" <<EOF
[binaries]
c = ['clang', '-arch', '$arch']
cpp = ['clang++', '-arch', '$arch']
ar = '/usr/bin/ar'
strip = '/usr/bin/strip'
pkg-config = '$(command -v pkg-config)'
[host_machine]
system = 'darwin'
cpu_family = '$family'
cpu = '$family'
endian = 'little'
EOF
  meson setup "$root/harfbuzz-build" "$src" --cross-file "$root/meson-$arch.ini" \
    --prefix "$prefix" --libdir lib --default-library static --buildtype release \
    -Dfreetype=disabled -Dglib=disabled -Dgobject=disabled -Dcairo=disabled -Dicu=disabled \
    -Dcoretext=disabled -Dchafa=disabled -Dtests=disabled -Ddocs=disabled -Dutilities=disabled \
    -Dintrospection=disabled
  ninja -C "$root/harfbuzz-build" -j"$jobs" install
  notices+=("harfbuzz:$src/$(field harfbuzz noticeFile)")

  src="$(unpack libass)"
  (cd "$src" && ./configure "${autotools[@]}" --disable-fontconfig --enable-coretext \
      --disable-require-system-font-provider && make -j"$jobs" && make install)
  notices+=("libass:$src/$(field libass noticeFile)")

  ffmpeg_flags+=(
    --enable-libmp3lame --enable-libass --enable-zlib --enable-videotoolbox --enable-audiotoolbox
    --pkg-config-flags=--static
    --extra-cflags="-I$prefix/include" --extra-ldflags="-L$prefix/lib"
  )
  unset CC CFLAGS CXX
fi

src="$(unpack ffmpeg)"
(cd "$src" && ./configure "${ffmpeg_flags[@]}" && make -j"$jobs" "$tool")
artifact="$out/$build_tag"
cp "$src/$tool" "$artifact"
chmod 0755 "$artifact"
mkdir -p "$out/$build_tag.notices"
cp "$src/COPYING.LGPLv2.1" "$out/$build_tag.notices/FFmpeg-COPYING.LGPLv2.1"
for entry in "${notices[@]+"${notices[@]}"}"; do
  cp "${entry#*:}" "$out/$build_tag.notices/${entry%%:*}-$(basename "${entry#*:}")"
done

# Linkage gate: only Apple system libraries and frameworks may be imported.
linkage="$(/usr/bin/otool -L "$artifact" | tail -n +2)"
if echo "$linkage" | awk '{print $1}' | grep -v -E '^(/usr/lib/|/System/Library/)' | grep -q .; then
  echo "non-system dynamic dependency in $artifact:" >&2; echo "$linkage" >&2; exit 1
fi
configuration="$(grep -m1 '^#define FFMPEG_CONFIGURATION' "$src/config.h" | sed 's/^#define FFMPEG_CONFIGURATION "//; s/"$//')"
case " $configuration " in *" --enable-gpl "*|*" --enable-nonfree "*) echo "GPL/nonfree configuration" >&2; exit 1 ;; esac
grep -q 'License: LGPL version 2.1 or later' "$src/ffbuild/config.log" 2>/dev/null \
  || grep -q '"LGPL version 2.1 or later"' "$src/config.h" \
  || { echo "configure did not report LGPL 2.1 or later" >&2; exit 1; }
version_output=""
if [ "$arch" = "$(uname -m)" ]; then version_output="$("$artifact" -hide_banner -version)"; fi

python3 - "$artifact" "$build_tag" "$variant" "$tool" "$arch" "$configuration" "$linkage" "$version_output" "$(clang --version | head -1)" <<'PY'
import hashlib, json, sys
path, tag, variant, tool, arch, configuration, linkage, version_output, compiler = sys.argv[1:10]
digest = hashlib.sha256(open(path, "rb").read()).hexdigest()
json.dump({
    "artifact": tag, "tool": tool, "variant": variant, "arch": arch,
    "sha256": digest, "configuration": configuration, "compiler": compiler,
    "linkage": [line.strip() for line in linkage.splitlines()],
    "versionOutput": version_output or "not executed (foreign architecture)",
}, open(path + ".evidence.json", "w"), indent=2)
print(f"{tag} sha256={digest}")
PY

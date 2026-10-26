#!/bin/bash
# Reproducible LGPL FFmpeg/FFprobe builds for the rightkit-ffmpeg artifact catalog (Windows x64).
# Windows twin of build-ffmpeg-macos.sh: same pinned sources, same LGPL-only posture.
#
# Toolchain: MSYS2 MINGW64 shell (MSYSTEM=MINGW64; msvcrt runtime, the one the approved decode
# build used) with mingw-w64-x86_64-gcc, make, nasm, diffutils, and for export-ass also
# autoconf, automake, libtool (zimg ships no configure). meson, ninja and a *native* Windows
# pkg-config (pkgconf) are named by MESON, NINJA and PKG_CONFIG, for example from a throwaway
# venv: `python -m venv v && v/Scripts/pip install meson ninja pkgconf`, then
#   MESON=v/Scripts/meson.exe NINJA=v/Scripts/ninja.exe \
#   PKG_CONFIG=v/Lib/site-packages/pkgconf/.bin/pkgconf.exe
# PYTHON names a Python 3 interpreter (MSYS2's base install has none). MSYS2's own
# /usr/bin/pkgconf is not usable: it is an MSYS program and hands meson POSIX paths.
#
#   build-ffmpeg-windows.sh fetch --variant <variant> --sources <dir>
#   build-ffmpeg-windows.sh build --variant <variant> --tool ffmpeg|ffprobe \
#       --sources <dir> --work <dir> --out <dir>
#       Verify pinned source digests, build, and write <out>/<tool>-<variant>-windows-x64.exe,
#       <same>.evidence.json, <same>.evidence/ (-version, -buildconf, -L, -encoders, -decoders,
#       -filters, objdump imports) and <same>.notices/.
#
# Variants
#   decode            The approved catalog build (third-party/BUILD-NOTES.txt): native codecs only.
#   export-ass-zimg   decode + libass (`subtitles`/`ass`) + FreeType/HarfBuzz (`drawtext`) + FriBidi
#                     + zimg (`zscale`) + zlib (PNG) + Media Foundation (h264_mf, hevc_mf, aac_mf).
#                     Same name and library set as the macOS export-ass-zimg variant; a source
#                     listed for `export-ass` in the manifests also belongs to it.
#
# Sources come from ffmpeg-sources.json (shared with macOS) plus ffmpeg-sources-windows.json
# (sources only the Windows build needs; macOS links the system zlib). Both variants keep
# --disable-gpl --disable-nonfree --disable-autodetect. Third-party libraries are built static
# from pinned sources into a private prefix and pkg-config is confined to that prefix. The
# finished exe may import only DLLs that live in %SystemRoot%\System32.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
cmd="${1:-}"
shift || true
variant="" tool="" sources="" work="" out=""
while [ $# -gt 0 ]; do
  case "$1" in
    --variant) variant="$2"; shift 2 ;;
    --tool) tool="$2"; shift 2 ;;
    --sources) sources="$2"; shift 2 ;;
    --work) work="$2"; shift 2 ;;
    --out) out="$2"; shift 2 ;;
    *) echo "unknown argument $1" >&2; exit 2 ;;
  esac
done
case "$variant" in decode|export-ass-zimg) ;; *) echo "--variant decode|export-ass-zimg" >&2; exit 2 ;; esac
test -n "$sources" || { echo "--sources <dir> is required" >&2; exit 2; }
python="${PYTHON:-python3}"
command -v "$python" >/dev/null || { echo "a Python 3 interpreter is required; set PYTHON" >&2; exit 1; }

manifests=("$here/ffmpeg-sources.json" "$here/ffmpeg-sources-windows.json")
field() { "$python" -c 'import json,sys
for m in sys.argv[3:]:
    s=json.load(open(m))["sources"]
    if sys.argv[1] in s:
        v=s[sys.argv[1]].get(sys.argv[2]); print("" if v is None else v); break' "$1" "$2" "${manifests[@]}" | tr -d '\r'; }
names() { "$python" -c 'import json,sys
want={sys.argv[1]} | ({"export-ass"} if sys.argv[1]=="export-ass-zimg" else set())
seen=set()
for m in sys.argv[2:]:
    for k,v in json.load(open(m))["sources"].items():
        if k not in seen and want & set(v["variants"]):
            seen.add(k); print(k)' "$variant" "${manifests[@]}" | tr -d '\r'; }
sha() { sha256sum "$1" | cut -d' ' -f1; }

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
case "$tool" in ffmpeg|ffprobe) ;; *) echo "--tool ffmpeg|ffprobe" >&2; exit 2 ;; esac
# shellcheck disable=SC2015  # each A && B is a test, so the || branch runs exactly when a requirement is missing
test -n "$work" && test -n "$out" || { echo "--work and --out are required" >&2; exit 2; }
test "${MSYSTEM:-}" = MINGW64 || { echo "run inside the MSYS2 MINGW64 shell (MSYSTEM=MINGW64)" >&2; exit 1; }
test "$(gcc -dumpmachine)" = x86_64-w64-mingw32 || { echo "x86_64-w64-mingw32 gcc required" >&2; exit 1; }

for name in $(names); do
  file="$sources/$(field "$name" file)"
  pinned="$(field "$name" sha256)"
  test -n "$pinned" || { echo "$name has no pinned sha256; run fetch and pin it first" >&2; exit 1; }
  test -f "$file" || { echo "missing source $file; run fetch first" >&2; exit 1; }
  test "$(sha "$file")" = "$pinned" || { echo "$name sha256 mismatch" >&2; exit 1; }
done

jobs="$(nproc)"
host=x86_64-w64-mingw32
build_tag="$tool-$variant-windows-x64"
root="$work/$build_tag"
rm -rf "$root"
mkdir -p "$root/src" "$root/prefix" "$root/tools" "$out"
# Native tools (meson, pkg-config, gcc) need drive-letter paths; MSYS tools accept them too.
prefix="$(cygpath -m "$root/prefix")"
unpack() {
  # The top-level directory comes from the tarball itself (GitHub tag archives are not
  # named <name>-<version>).
  local file
  file="$sources/$(field "$1" file)"
  tar -xf "$file" -C "$root/src"
  echo "$root/src/$(tar -tf "$file" | awk -F/ 'NR==1{print $1}')"
}

ffmpeg_flags=(
  --disable-gpl --disable-nonfree --disable-autodetect
  --enable-static --disable-shared --disable-doc --disable-debug --disable-ffplay --enable-small
  --arch=x86_64 --target-os=mingw32
)

notices=()
tools="$(gcc --version | head -1); $(nasm -v); $(make --version | head -1)"
if [ "$variant" = export-ass-zimg ]; then
  for v in MESON NINJA PKG_CONFIG; do
    # shellcheck disable=SC2015  # each A && B is a test, so the || branch runs exactly when a requirement is missing
    test -n "${!v:-}" && test -x "${!v}" || { echo "$v must name an executable (see header)" >&2; exit 1; }
  done
  for t in autoconf automake libtoolize; do
    command -v "$t" >/dev/null || { echo "$t is required (zimg)" >&2; exit 1; }
  done
  # FFmpeg's configure and the autotools builds look for `pkg-config` on PATH; give them the
  # native one without putting a host path into FFmpeg's configuration string.
  cp "$PKG_CONFIG" "$root/tools/pkg-config.exe"
  export PATH="$root/tools:$PATH"
  PKG_CONFIG="$(cygpath -m "$root/tools/pkg-config.exe")"
  export PKG_CONFIG
  export PKG_CONFIG_LIBDIR="$prefix/lib/pkgconfig"
  export PKG_CONFIG_PATH="$PKG_CONFIG_LIBDIR"
  NINJA="$(cygpath -m "$NINJA")"
  export NINJA
  autotools=(--host="$host" --build="$host" --prefix="$prefix" --disable-shared --enable-static)
  cat > "$root/meson-native.ini" <<EOF
[binaries]
c = 'gcc'
cpp = 'g++'
ar = 'ar'
strip = 'strip'
pkg-config = '$PKG_CONFIG'
EOF
  mesonbuild() {
    "$MESON" setup "$(cygpath -m "$1")" "$(cygpath -m "$2")" --native-file "$(cygpath -m "$root/meson-native.ini")" \
      --prefix "$prefix" --libdir lib --default-library static --buildtype release "${@:3}"
    "$NINJA" -C "$(cygpath -m "$1")" -j"$jobs" install
  }
  export CFLAGS="-O2" CXXFLAGS="-O2"

  src="$(unpack zlib)"
  # Static library only: the upstream `install` target also builds and installs zlib1.dll.
  (cd "$src" && make -f win32/Makefile.gcc -j"$jobs" libz.a \
    && mkdir -p "$root/prefix/include" "$root/prefix/lib/pkgconfig" \
    && cp zlib.h zconf.h "$root/prefix/include/" && cp libz.a "$root/prefix/lib/" \
    && sed -e "s|@prefix@|$prefix|g" -e "s|@exec_prefix@|$prefix|g" -e "s|@libdir@|$prefix/lib|g" \
         -e "s|@sharedlibdir@|$prefix/lib|g" -e "s|@includedir@|$prefix/include|g" \
         -e "s|@VERSION@|$(field zlib version)|g" zlib.pc.in > "$root/prefix/lib/pkgconfig/zlib.pc")
  notices+=("zlib:$src/$(field zlib noticeFile)")

  src="$(unpack freetype)"
  (cd "$src" && ./configure "${autotools[@]}" --without-zlib --without-bzip2 --without-png \
      --without-harfbuzz --without-brotli && make -j"$jobs" && make install)
  notices+=("freetype:$src/$(field freetype noticeFile)")

  src="$(unpack fribidi)"
  # Keep only files present in the upstream git tag (see the macOS recipe): drop autotools
  # output and the pre-generated tables so meson regenerates them from the tagged Unicode data.
  (cd "$src" && rm -f configure aclocal.m4 config.h.in ltmain.sh ar-lib compile config.guess \
      config.sub depcomp install-sh missing test-driver ChangeLog INSTALL \
      lib/arabic-shaping.tab.i lib/bidi-type.tab.i lib/brackets-type.tab.i lib/brackets.tab.i \
      lib/joining-type.tab.i lib/mirroring.tab.i lib/fribidi-unicode-version.h doc/*.3 doc/c2man.stamp && find . -name Makefile.in -delete \
      && rm -f m4/libtool.m4 m4/ltoptions.m4 m4/ltsugar.m4 m4/ltversion.m4 'm4/lt~obsolete.m4')
  mesonbuild "$root/fribidi-build" "$src" -Ddocs=false -Dbin=false -Dtests=false -Ddeprecated=true
  notices+=("fribidi:$src/$(field fribidi noticeFile)")

  src="$(unpack harfbuzz)"
  # FreeType integration (hb-ft) is required by FFmpeg's drawtext filter.
  mesonbuild "$root/harfbuzz-build" "$src" \
    -Dfreetype=enabled -Dglib=disabled -Dgobject=disabled -Dcairo=disabled -Dicu=disabled \
    -Dgraphite=disabled -Dgraphite2=disabled -Dcoretext=disabled -Dgdi=disabled -Ddirectwrite=disabled \
    -Dchafa=disabled -Dwasm=disabled -Dtests=disabled \
    -Ddocs=disabled -Dutilities=disabled -Dintrospection=disabled -Dbenchmark=disabled
  notices+=("harfbuzz:$src/$(field harfbuzz noticeFile)")

  src="$(unpack libass)"
  # DirectWrite is the system font provider (the Windows counterpart of CoreText; dwrite.dll is
  # loaded at run time). ac_cv_header_iconv_h=no keeps MSYS2's own libiconv out of the link.
  (cd "$src" && ./configure "${autotools[@]}" --disable-fontconfig --enable-directwrite \
      --disable-require-system-font-provider ac_cv_header_iconv_h=no && make -j"$jobs" && make install)
  grep -q '^#define CONFIG_ICONV' "$src/config.h" && { echo "libass picked up iconv" >&2; exit 1; }
  grep -q '^#define CONFIG_DIRECTWRITE 1' "$src/config.h" || { echo "libass has no DirectWrite provider" >&2; exit 1; }
  notices+=("libass:$src/$(field libass noticeFile)")

  src="$(unpack zimg)"
  (cd "$src" && autoreconf --install --force && ./configure "${autotools[@]}" \
      && make -j"$jobs" && make install)
  notices+=("zimg:$src/$(field zimg noticeFile)")

  ffmpeg_flags+=(
    --enable-libass --enable-libfreetype --enable-libharfbuzz --enable-libzimg
    --enable-zlib --enable-mediafoundation
    --pkg-config-flags=--static
  )
  tools="$tools; meson $("$MESON" --version); ninja $("$NINJA" --version); pkg-config $(pkg-config --version); $(autoconf --version | head -1); $(automake --version | head -1); $(libtoolize --version | head -1)"
  unset CFLAGS CXXFLAGS
fi
# Fully static: no libwinpthread-1.dll, libgcc or libstdc++ DLL. The PE timestamp is zeroed so
# two builds of the same sources are comparable: --no-insert-timestamp covers the link, and
# SOURCE_DATE_EPOCH covers FFmpeg's `strip` step, which otherwise stamps the current time.
ffmpeg_flags+=(--extra-ldflags="-static -static-libgcc -Wl,--no-insert-timestamp")
export SOURCE_DATE_EPOCH=0

src="$(unpack ffmpeg)"
(cd "$src" && ./configure "${ffmpeg_flags[@]}" && make -j"$jobs" "$tool.exe")
artifact="$out/$build_tag.exe"
cp "$src/$tool.exe" "$artifact"
rm -rf "$out/$build_tag.notices" "$out/$build_tag.evidence"
mkdir -p "$out/$build_tag.notices" "$out/$build_tag.evidence"
cp "$src/COPYING.LGPLv2.1" "$out/$build_tag.notices/FFmpeg-COPYING.LGPLv2.1"
for entry in "${notices[@]+"${notices[@]}"}"; do
  cp "${entry#*:}" "$out/$build_tag.notices/${entry%%:*}-$(basename "${entry#*:}")"
done
# -static also links toolchain runtime code into every exe: the mingw-w64 C runtime, winpthreads
# and libgcc, plus libstdc++ when a C++ library (zimg, HarfBuzz) is linked. The mingw-w64 runtime
# licence requires its notices with binaries; they come from the toolchain's own licence files.
licenses=/mingw64/share/licenses
toolchain_notice() {
  # Package directory names vary between MSYS2 snapshots (crt or crt-git, winpthreads or
  # libwinpthread, gcc or gcc-libs), so each file is found by pattern.
  local found
  # shellcheck disable=SC2012,SC2086  # ls over build-output names this script controls (no spaces); the order of the listing is what is wanted; unquoted on purpose: the argument is a glob or a word list
  found="$(ls $1 2>/dev/null | awk 'NR==1' || true)"
  if [ -z "$found" ]; then
    # shellcheck disable=SC2012  # ls over build-output names this script controls (no spaces); the order of the listing is what is wanted
    echo "toolchain licence file not found: $1; $licenses holds: $(ls "$licenses" 2>/dev/null | tr '
' ' ')" >&2
    exit 1
  fi
  cp "$found" "$out/$build_tag.notices/$2"
}
toolchain_notice "$licenses/*crt*/COPYING.MinGW-w64-runtime.txt $licenses/*headers*/COPYING.MinGW-w64-runtime.txt" MinGW-w64-runtime-COPYING.txt
toolchain_notice "$licenses/*winpthread*/COPYING" winpthreads-COPYING.txt
toolchain_notice "$licenses/gcc*/COPYING.RUNTIME" GCC-RUNTIME-LIBRARY-EXCEPTION.txt

# Linkage gate: every imported DLL must be a Windows system DLL.
imports="$(objdump -p "$artifact" | sed -n 's/^[[:space:]]*DLL Name: //p' | tr -d '\r' | sort -f)"
system32="$(cygpath -u "$SYSTEMROOT")/System32"
for dll in $imports; do
  test -f "$system32/$dll" || { echo "non-system import $dll in $artifact" >&2; exit 1; }
  # shellcheck disable=SC2018,SC2019  # ASCII case mapping on purpose: DLL names are ASCII
  case "$(echo "$dll" | tr 'A-Z' 'a-z')" in
    libwinpthread*|libgcc*|libstdc*|msys-*|libiconv*|zlib1*) echo "toolchain DLL import $dll" >&2; exit 1 ;;
  esac
done
configuration="$(grep -m1 '^#define FFMPEG_CONFIGURATION' "$src/config.h" | sed 's/^#define FFMPEG_CONFIGURATION "//; s/"$//')"
case " $configuration " in *" --enable-gpl "*|*" --enable-nonfree "*|*" --enable-version3 "*) echo "GPL/nonfree/version3 configuration" >&2; exit 1 ;; esac
grep -q '"LGPL version 2.1 or later"' "$src/config.h" || { echo "configure did not report LGPL 2.1 or later" >&2; exit 1; }
# shellcheck disable=SC2015  # each A && B is a test, so the || branch runs exactly when a requirement is missing
grep -q '^#define CONFIG_GPL 0' "$src/config.h" && grep -q '^#define CONFIG_NONFREE 0' "$src/config.h" \
  || { echo "CONFIG_GPL/CONFIG_NONFREE not 0" >&2; exit 1; }

ev="$out/$build_tag.evidence"
echo "$configuration" > "$ev/configure-line.txt"
echo "$imports" > "$ev/imports.txt"
# The toolchain packages whose runtime code is linked statically, with the digest of each package
# file: the catalog row's toolchain runtime components record these. The digest comes from the
# package file pacman still has cached, or else from pacman's sync database entry for exactly the
# installed version (a CI runner keeps no package files).
{
  pacman -Q 2>/dev/null | grep -E '^mingw-w64-x86_64-(gcc|gcc-libs|binutils|(crt|headers|winpthreads|libwinpthread)(-git)?) ' |
    while read -r name version; do
      digest="$(cd /var/cache/pacman/pkg 2>/dev/null && sha256sum "$name-$version-any.pkg.tar.zst" 2>/dev/null | cut -d' ' -f1 || true)"
      if [ -z "$digest" ]; then
        digest="$(bsdtar -xOf /var/lib/pacman/sync/mingw64.db "$name-$version/desc" 2>/dev/null |
          awk '/^%SHA256SUM%$/ { getline; print; exit }' || true)"
      fi
      echo "$name $version ${digest:-digest-unavailable}"
    done || true
} > "$ev/toolchain-packages.txt"
for probe in version buildconf L encoders decoders filters; do
  "$artifact" -hide_banner "-$probe" < /dev/null 2>&1 | tr -d '\r' > "$ev/$probe.txt"
done
if grep -E -- '--enable-(gpl|nonfree|version3)' "$ev/buildconf.txt" "$ev/version.txt"; then
  echo "GPL/nonfree/version3 flag in the built binary" >&2; exit 1
fi
grep -q 'GNU Lesser General Public' "$ev/L.txt" || { echo "-L does not report the LGPL" >&2; exit 1; }
if [ "$variant" = export-ass-zimg ] && [ "$tool" = ffmpeg ]; then
  # What CutRight's export pipeline needs; `eq` is GPL-only and must never appear.
  for f in zscale tonemap overlay loudnorm drawtext freezedetect blackdetect ebur128 silencedetect \
      lut3d signalstats astats pad colorspace amix tpad colorbalance colortemperature lutrgb lutyuv \
      adelay transpose highpass alimiter subtitles ass scale; do
    grep -qE "^ [A-Z.]{3} $f " "$ev/filters.txt" || { echo "required filter $f is missing" >&2; exit 1; }
  done
  grep -qE "^ [A-Z.]{3} eq " "$ev/filters.txt" && { echo "GPL-only filter eq is present" >&2; exit 1; }
  for e in h264_mf aac png; do
    grep -qE "^ [A-Z.]{6} $e " "$ev/encoders.txt" || { echo "required encoder $e is missing" >&2; exit 1; }
  done
  grep -qE "^ [A-Z.]{6} (libx264|libx265|libopenh264)" "$ev/encoders.txt" && { echo "unexpected external encoder" >&2; exit 1; }
fi

"$python" - "$(cygpath -m "$artifact")" "$build_tag" "$variant" "$tool" "$configuration" "$imports" "$tools" <<'PY'
import hashlib, json, os, sys
path, tag, variant, tool, configuration, imports, tools = sys.argv[1:8]
digest = hashlib.sha256(open(path, "rb").read()).hexdigest()
version = open(os.path.splitext(path)[0] + ".evidence/version.txt").read()
json.dump({
    "artifact": tag, "tool": tool, "variant": variant, "arch": "x86_64",
    "sha256": digest, "size": os.path.getsize(path), "configuration": configuration, "tools": tools,
    "imports": imports.split(),
    "versionOutput": version,
}, open(os.path.splitext(path)[0] + ".evidence.json", "w", newline="\n"), indent=2)
print(f"{tag} sha256={digest} size={os.path.getsize(path)}")
PY

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
#   export-ass       decode + libass (`subtitles`/`ass` burn-in) + FreeType/HarfBuzz
#                    (`drawtext` captions) + zlib (PNG) + VideoToolbox/AudioToolbox
#                    (h264_videotoolbox).
#   export-ass-zimg  export-ass + libzimg (the `zscale` filter: PQ/HLG linearise, bt2020 to
#                    bt709, dither; needed for HDR and Apple Log to Rec.709 with `tonemap`).
#                    zimg 3.0.5 is WTFPL, statically linked; configure takes no GPL, nonfree
#                    or version3 switch for it. A separate variant so the digest and licence
#                    set of the approved export-ass rows never change meaning.
#
# export-ass and export-ass-zimg need meson and ninja for HarfBuzz and FriBidi (on PATH, or named by MESON and
# NINJA) and nasm for x86_64. Both arches are built with an explicit meson cross file (`-arch <arch>`); meson's
# native (build-machine) compiler stays the host clang so FriBidi's table generators run on
# the host. FriBidi is built only from files that exist in its upstream v<version> git tag:
# the release tarball's autotools output and pre-generated tables are deleted and the tables
# are regenerated from the tagged Unicode data (FriBidi publishes no tarball checksum).
#
# All variants keep --disable-gpl --disable-nonfree --disable-autodetect. Third-party
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
case "$variant" in decode|export-ass|export-ass-zimg) ;; *) echo "--variant decode|export-ass|export-ass-zimg" >&2; exit 2 ;; esac
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
# A source whose archive unpacks to a differently named directory (zimg: zimg-release-3.0.5)
# names it in `dirName`.
unpack() {
  tar -xf "$sources/$(field "$1" file)" -C "$root/src"
  local dir; dir="$(field "$1" dirName)"
  echo "$root/src/${dir:-$1-$(field "$1" version)}"
}

ffmpeg_flags=(
  --disable-gpl --disable-nonfree --disable-autodetect
  --enable-static --disable-shared --disable-doc --disable-debug --disable-ffplay --enable-small
  --arch="$arch" --cc="$cc"
)
if [ "$arch" = x86_64 ]; then
  ffmpeg_flags+=(--enable-cross-compile --target-os=darwin)
fi

notices=()
if [ "$variant" != decode ]; then
  # MESON/NINJA may name a specific meson/ninja (for example from a throwaway venv).
  meson="${MESON:-meson}" ninja="${NINJA:-ninja}"
  command -v "$meson" >/dev/null || { echo "meson is required (harfbuzz, fribidi); set MESON" >&2; exit 1; }
  command -v "$ninja" >/dev/null || { echo "ninja is required (harfbuzz, fribidi); set NINJA" >&2; exit 1; }
  export NINJA="$(command -v "$ninja")"
  command -v pkg-config >/dev/null || { echo "pkg-config is required" >&2; exit 1; }
  export PKG_CONFIG_LIBDIR="$prefix/lib/pkgconfig"
  unset PKG_CONFIG_PATH
  export ZERO_AR_DATE=1
  autotools=(--host="$host" --prefix="$prefix" --disable-shared --enable-static)
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
  # meson reads CC/CXX/CFLAGS for the build machine when cross compiling: keep them unset.
  mesonbuild() {
    env -u CC -u CXX -u CFLAGS -u CXXFLAGS "$meson" setup "$1" "$2" --cross-file "$root/meson-$arch.ini" \
      --prefix "$prefix" --libdir lib --default-library static --buildtype release "${@:3}"
    env -u CC -u CXX -u CFLAGS -u CXXFLAGS "$NINJA" -C "$1" -j"$jobs" install
  }
  export CC="$cc" CFLAGS="-O2" CXX="clang++ -arch $arch"

  src="$(unpack freetype)"
  (cd "$src" && ./configure "${autotools[@]}" --without-zlib --without-bzip2 --without-png \
      --without-harfbuzz --without-brotli && make -j"$jobs" && make install)
  notices+=("freetype:$src/$(field freetype noticeFile)")

  src="$(unpack fribidi)"
  # Keep only files present in the upstream git tag (see header): drop autotools output and
  # the pre-generated tables so meson regenerates them from the tagged Unicode data.
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
    -Dgraphite=disabled -Dcoretext=disabled -Dchafa=disabled -Dwasm=disabled -Dtests=disabled \
    -Ddocs=disabled -Dutilities=disabled -Dintrospection=disabled -Dbenchmark=disabled
  notices+=("harfbuzz:$src/$(field harfbuzz noticeFile)")

  src="$(unpack libass)"
  (cd "$src" && ./configure "${autotools[@]}" --disable-fontconfig --enable-coretext \
      --disable-require-system-font-provider && make -j"$jobs" && make install)
  notices+=("libass:$src/$(field libass noticeFile)")

  ffmpeg_flags+=(
    --enable-libass --enable-libfreetype --enable-libharfbuzz
    --enable-zlib --enable-videotoolbox --enable-audiotoolbox
    --pkg-config-flags=--static
  )

  if [ "$variant" = export-ass-zimg ]; then
    # zimg 3.0.5 has no configure script in its tag archive and the build host needs no
    # autotools: the library is compiled straight from the source lists in the tagged
    # Makefile.am (so an upstream source list stays the single authority), with the same
    # flags Makefile.am and configure.ac apply (-std=c++14 -O2 -DNDEBUG -fvisibility=hidden,
    # per-ISA -m flags on x86_64, NEON on aarch64, no -march for aarch64).
    src="$(unpack zimg)"
    zimg_build="$root/zimg-build"
    mkdir -p "$zimg_build" "$prefix/lib/pkgconfig" "$prefix/include"
    zimg_defs=()
    zimg_tune() { echo 'int main(){return 0;}' | clang -arch "$arch" -x c++ - -o /dev/null "$1" >/dev/null 2>&1 && echo "$1"; }
    if [ "$family" = aarch64 ]; then
      zimg_defs+=(-DZIMG_ARM)
    else
      zimg_defs+=(-DZIMG_X86)
      # configure.ac defines ZIMG_X86_AVX512 only when the compiler takes this whole flag set.
      if printf '#include <immintrin.h>\nint main(){volatile __m512 x=_mm512_setzero_ps(); x=_mm512_add_ps(x,x); return 0;}\n' \
          | clang -arch "$arch" -x c++ - -o /dev/null -mavx512f -mavx512pf -mavx512er -mavx512cd -mavx512vl -mavx512bw -mavx512dq -mavx512ifma -mavx512vbmi -mavx512vbmi2 -mavx512bitalg -mavx512vpopcntdq -mavx512vnni >/dev/null 2>&1; then
        zimg_defs+=(-DZIMG_X86_AVX512)
      fi
    fi
    # Emit "<source>|<extra flags>" for every .cpp the tagged Makefile.am lists for this CPU.
    python3 -I - "$src/Makefile.am" "$family" "${zimg_defs[*]}" \
        "$(zimg_tune -mtune=sandybridge)" "$(zimg_tune -mtune=ivybridge)" "$(zimg_tune -mtune=haswell)" \
        "$(zimg_tune -mtune=skylake-avx512)" "$(zimg_tune -mtune=cascadelake)" > "$zimg_build/sources.list" <<'PY'
import re, sys
makefile, family, defs, snb, ivb, hsw, skx, clx = sys.argv[1:9]
arm, x86, avx512 = family == "aarch64", family == "x86_64", "-DZIMG_X86_AVX512" in defs
active = [True]
vars_ = {}
cond = {"ARMSIMD": arm, "X86SIMD": x86, "X86SIMD_AVX512": avx512}
text = open(makefile).read().replace("\\\n", " ")
for line in text.splitlines():
    line = line.strip()
    m = re.match(r"if (\w+)$", line)
    if m:
        active.append(cond.get(m.group(1), False) and active[-1]); continue
    if line.startswith("endif"):
        active.pop(); continue
    m = re.match(r"(\w+)\s*(\+?=)\s*(.*)$", line)
    if m and all(active):
        name, op, value = m.groups()
        vars_[name] = (vars_.get(name, "") + " " + value) if op == "+=" else value
base = "-std=c++14 -O2 -Wall -Wextra -Wno-unused-parameter -Wno-missing-field-initializers -DNDEBUG -fvisibility=hidden"
tune = {"$(SNB_CFLAGS)": snb, "$(IVB_CFLAGS)": ivb, "$(HSW_CFLAGS)": hsw, "$(SKX_CFLAGS)": skx, "$(CLX_CFLAGS)": clx,
        "$(NEON_CFLAGS)": "", "$(AM_CXXFLAGS)": ""}
def flags(lib):
    value = vars_.get(lib + "_CXXFLAGS", "")
    for k, v in tune.items():
        value = value.replace(k, v)
    return " ".join(value.split())
groups = ["libzimg_internal_la"]
if arm: groups.append("libneon_la")
if x86: groups += ["libsse_la", "libsse2_la", "libavx_la", "libf16c_la", "libavx2_la"]
if avx512: groups += ["libavx512_la", "libavx512_vnni_la"]
count = 0
for g in groups:
    for source in vars_[g + "_SOURCES"].split():
        if source.endswith(".cpp"):
            print(f"{source}|{base} {flags(g)}"); count += 1
sys.stderr.write(f"zimg sources: {count}\n")
PY
    zimg_objs=()
    n=0
    while IFS='|' read -r zsrc zflags; do
      n=$((n + 1))
      obj="$zimg_build/$(printf '%03d' "$n")-$(basename "$zsrc" .cpp).o"
      # shellcheck disable=SC2086
      (cd "$src" && clang++ -arch "$arch" $zflags "${zimg_defs[@]}" -I src/zimg -c "$zsrc" -o "$obj")
      zimg_objs+=("$obj")
    done < "$zimg_build/sources.list"
    rm -f "$prefix/lib/libzimg.a"
    /usr/bin/ar rcs "$prefix/lib/libzimg.a" "${zimg_objs[@]}"
    cp "$src/src/zimg/api/zimg.h" "$src/src/zimg/api/zimg++.hpp" "$prefix/include/"
    # zimg.pc.in with configure's substitutions; the C++ runtime on macOS is libc++.
    sed -e "s|@prefix@|$prefix|; s|@exec_prefix@|$prefix|; s|@libdir@|$prefix/lib|; s|@includedir@|$prefix/include|" \
        -e "s|@VERSION@|$(field zimg version)|; s|@STL_LIBS@|-lc++|" "$src/zimg.pc.in" > "$prefix/lib/pkgconfig/zimg.pc"
    notices+=("zimg:$src/$(field zimg noticeFile)")
    ffmpeg_flags+=(--enable-libzimg)
  fi
  # Include and library paths come only from the prefix's .pc files, so the embedded
  # configuration string (and therefore the binary) does not depend on the work directory.
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
tools="clang: $(clang --version | head -1)"
if [ "$variant" != decode ]; then
  tools="$tools; meson $("$meson" --version); ninja $("$NINJA" --version); pkg-config $(pkg-config --version)"
fi
if [ "$arch" = x86_64 ]; then tools="$tools; $(nasm -v)"; fi

python3 - "$artifact" "$build_tag" "$variant" "$tool" "$arch" "$configuration" "$linkage" "$version_output" "$tools" <<'PY'
import hashlib, json, os, sys
path, tag, variant, tool, arch, configuration, linkage, version_output, tools = sys.argv[1:10]
digest = hashlib.sha256(open(path, "rb").read()).hexdigest()
json.dump({
    "artifact": tag, "tool": tool, "variant": variant, "arch": arch,
    "sha256": digest, "size": os.path.getsize(path), "configuration": configuration, "tools": tools,
    "linkage": [line.strip() for line in linkage.splitlines()],
    "versionOutput": version_output or "not executed (foreign architecture)",
}, open(path + ".evidence.json", "w"), indent=2)
print(f"{tag} sha256={digest}")
PY

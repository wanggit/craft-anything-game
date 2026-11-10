#!/bin/sh
# Build crn2blocks (this folder's glue) against Unity's open-source crunch fork.
# One-time setup for the unity-texture-codec engine: needs git, python3 and a C++ compiler.
# The upstream source is cloned here at build time and never ships with Craft Anything.
set -e
cd "$(dirname "$0")"
CRUNCH=unity-crunch-etc
if [ ! -d "$CRUNCH" ]; then
  git clone --depth 1 --branch unity https://github.com/Unity-Technologies/crunch "$CRUNCH"
fi
cd "$CRUNCH"

python3 -I - <<'EOF'
import re

def patch(path, old, new, must=True):
    s = open(path).read()
    if new in s:
        print(f"{path}: already patched")
        return
    if old not in s:
        if must:
            raise SystemExit(f"{path}: pattern not found: {old[:60]!r}")
        print(f"{path}: skip")
        return
    open(path, "w").write(s.replace(old, new))
    print(f"{path}: patched")

patch("inc/crn_decomp.h",
      "#ifdef __x86_64__\n      typedef uint64 ptr_bits;",
      "#if defined(__x86_64__) || defined(__aarch64__) || defined(__arm64__) || defined(_M_X64)\n      typedef uint64 ptr_bits;", must=False)
patch("inc/crn_decomp.h",
      "if ((uint32)reinterpret_cast<ptr_bits>(p) & (CRND_MIN_ALLOC_ALIGNMENT - 1))",
      "if (reinterpret_cast<ptr_bits>(p) & (CRND_MIN_ALLOC_ALIGNMENT - 1))", must=False)
patch("inc/crn_decomp.h",
      "CRND_ASSERT(((uint32)p_new & (CRND_MIN_ALLOC_ALIGNMENT - 1)) == 0);",
      "CRND_ASSERT(((ptr_bits)p_new & (CRND_MIN_ALLOC_ALIGNMENT - 1)) == 0);", must=False)
patch("crnlib/crn_platform.h",
      '#define CRNLIB_BREAKPOINT asm("int $3");',
      '#if defined(__x86_64__) || defined(__i386__)\n   #define CRNLIB_BREAKPOINT asm("int $3");\n#else\n   #define CRNLIB_BREAKPOINT __builtin_trap();\n#endif', must=False)
patch("crnlib/crn_vector.cpp",
      "math::is_power_of_2(new_capacity)",
      "math::is_power_of_2((uint32)new_capacity)", must=False)
patch("crnlib/crn_vector.cpp",
      "new_capacity = math::next_pow2(new_capacity);",
      "new_capacity = math::next_pow2((uint32)new_capacity);", must=False)
EOF

CXX="${CXX:-c++}"
command -v "$CXX" >/dev/null 2>&1 || CXX=clang++
command -v "$CXX" >/dev/null 2>&1 || CXX=g++
mkdir -p obj
ok=1
for f in crn_assert crn_mem crn_platform crn_vector crn_symbol_codec crn_prefix_coding crn_hash crn_core crn_dynamic_string crn_data_stream crn_checksum crn_strutils crn_huffman_codes; do
  "$CXX" -O2 -std=c++11 -w -I ../kit-shim -I inc -I crnlib -c "crnlib/$f.cpp" -o "obj/$f.o" || { echo "FAIL $f"; ok=0; }
done
"$CXX" -O2 -std=c++11 -w -I ../kit-shim -I inc -I crnlib -c emscripten/crunch_lib.cpp -o obj/crunch_lib.o || { echo FAIL crunch_lib; ok=0; }
"$CXX" -O2 -std=c++11 -w -I ../kit-shim -I inc -c ../crn2blocks.cpp -o obj/crn2blocks.o || { echo FAIL crn2blocks; ok=0; }
[ "$ok" = 1 ] || exit 1
"$CXX" -O2 -o ../crn2blocks \
  obj/crn_assert.o obj/crn_mem.o obj/crn_platform.o obj/crn_vector.o \
  obj/crn_symbol_codec.o obj/crn_prefix_coding.o obj/crn_hash.o obj/crn_core.o \
  obj/crn_dynamic_string.o obj/crn_data_stream.o obj/crn_checksum.o \
  obj/crn_strutils.o obj/crn_huffman_codes.o obj/crunch_lib.o obj/crn2blocks.o
echo BUILD_OK

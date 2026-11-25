#!/bin/sh
# Source this file to build VIctory on Linux for Windows (x86_64-pc-windows-msvc)
# without Microsoft's SDK, and to run its tests under Wine:
#
#     . tools/linux/cross-env.sh
#     cargo build --release -p victory --target x86_64-pc-windows-msvc
#     cargo test --workspace --target x86_64-pc-windows-msvc
#
# Needs: rustup (the pinned toolchain has the target), lld (lld-link), llvm
# (llvm-lib), clang (clang-cl) and Wine with its Windows import libraries
# (lib/wine/x86_64-windows/lib*.a; Arch's `wine` package has them).
#
# How: Wine ships import libraries for the DLLs it implements, the C runtime
# among them, and lld-link takes them as it takes Microsoft's. They are
# linked under the names the Rust target asks for, plus the few symbols of
# Microsoft's startup code that Wine's does not define. See docs/linux.md.
#
# Variables read: VICTORY_WINE_LIBS (the folder with Wine's lib*.a),
# VICTORY_CROSS_DIR (where the links are kept), VICTORY_TEST_WINE (what runs
# test executables: `wine` by default; tools/linux/proton-wine.sh for Proton).

_victory_fail() { echo "cross-env.sh: $*" >&2; return 1; }

_victory_libs=${VICTORY_WINE_LIBS:-}
if [ -z "$_victory_libs" ]; then
    for _d in /usr/lib/wine/x86_64-windows /usr/lib64/wine/x86_64-windows \
        /usr/lib/x86_64-linux-gnu/wine/x86_64-windows /opt/wine-stable/lib64/wine/x86_64-windows \
        /opt/wine-devel/lib64/wine/x86_64-windows /opt/wine-staging/lib64/wine/x86_64-windows; do
        if [ -f "$_d/libkernel32.a" ]; then _victory_libs=$_d; break; fi
    done
fi
_victory_dir=${VICTORY_CROSS_DIR:-${XDG_CACHE_HOME:-$HOME/.cache}/victory-cross}

if [ ! -f "$_victory_libs/libkernel32.a" ] || [ ! -f "$_victory_libs/libwinecrt0.a" ]; then
    _victory_fail "Wine's import libraries not found; set VICTORY_WINE_LIBS to the folder with libkernel32.a"
elif ! command -v lld-link >/dev/null || ! command -v llvm-lib >/dev/null || ! command -v clang-cl >/dev/null; then
    _victory_fail "lld-link, llvm-lib and clang-cl are needed (packages lld, llvm, clang)"
else
    mkdir -p "$_victory_dir/lib" || _victory_fail "cannot make $_victory_dir"
    # kernel32.lib -> libkernel32.a, and so on.
    for _f in "$_victory_libs"/lib*.a; do
        _n=${_f##*/lib}
        ln -sf "$_f" "$_victory_dir/lib/${_n%.a}.lib"
    done
    # The names Microsoft's toolchain has for the C runtime's parts, and the
    # delay-load helper (in Wine's winecrt0).
    ln -sf "$_victory_libs/libucrtbase.a" "$_victory_dir/lib/ucrt.lib"
    ln -sf "$_victory_libs/libvcruntime140.a" "$_victory_dir/lib/vcruntime.lib"
    ln -sf "$_victory_libs/libwinecrt0.a" "$_victory_dir/lib/delayimp.lib"
    # What Wine's startup code leaves to the toolchain: the bounds of the
    # initializer tables it walks, and the entry point of a DLL under the
    # name Microsoft's linker looks for.
    if [ ! -f "$_victory_dir/lib/victorycrt.lib" ]; then
        cat > "$_victory_dir/victorycrt.c" <<'CEOF'
typedef void (*pvfv)(void);
typedef int (*pifv)(void);
#pragma section(".CRT$XIA", read)
#pragma section(".CRT$XIZ", read)
#pragma section(".CRT$XCA", read)
#pragma section(".CRT$XCZ", read)
#pragma section(".CRT$XTA", read)
#pragma section(".CRT$XTZ", read)
__declspec(allocate(".CRT$XIA")) pifv __xi_a[] = { 0 };
__declspec(allocate(".CRT$XIZ")) pifv __xi_z[] = { 0 };
__declspec(allocate(".CRT$XCA")) pvfv __xc_a[] = { 0 };
__declspec(allocate(".CRT$XCZ")) pvfv __xc_z[] = { 0 };
__declspec(allocate(".CRT$XTA")) pvfv __xt_a[] = { 0 };
__declspec(allocate(".CRT$XTZ")) pvfv __xt_z[] = { 0 };
int __stdcall DllMainCRTStartup(void *, unsigned long, void *);
int __stdcall _DllMainCRTStartup(void *h, unsigned long r, void *p) { return DllMainCRTStartup(h, r, p); }
CEOF
        ( cd "$_victory_dir" &&
            clang-cl --target=x86_64-pc-windows-msvc /c /O2 /Zl victorycrt.c /Fovictorycrt.obj &&
            llvm-lib /out:lib/victorycrt.lib victorycrt.obj >/dev/null ) ||
            _victory_fail "could not build the startup shim in $_victory_dir"
    fi
    _victory_inc=${_victory_libs%/lib*/wine/x86_64-windows}/include/wine
    [ -d "$_victory_inc/msvcrt" ] || _victory_inc=/usr/include/wine

    export CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER=lld-link
    export CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS="-Lnative=$_victory_dir/lib -Clink-arg=victorycrt.lib -Clink-arg=winecrt0.lib -Clink-arg=ucrtbase.lib -Clink-arg=vcruntime140.lib"
    # C in dependencies (blake3's) is built by clang-cl against Wine's headers.
    export CC_x86_64_pc_windows_msvc=clang-cl AR_x86_64_pc_windows_msvc=llvm-lib
    export CFLAGS_x86_64_pc_windows_msvc="-imsvc $_victory_inc/msvcrt -imsvc $_victory_inc/windows"
    # `cargo test` and `cargo run` start what they built under Wine.
    export CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUNNER="${VICTORY_TEST_WINE:-wine}"
    export WINEDEBUG="${WINEDEBUG:--all}"
    echo "cross-env.sh: x86_64-pc-windows-msvc links against $_victory_libs; tests run with $CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUNNER"
fi
unset _victory_libs _victory_dir _victory_inc _victory_fail _d _f _n

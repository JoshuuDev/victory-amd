# Linux

VIctory is a Windows program and stays one: the title's code is Windows
x86-64 code that runs inside our process, on Windows threads, memory calls
and exception handling. On Linux it runs under Wine, with vkd3d-proton as
its Direct3D 12 (what Proton ships), built on Linux for Windows.

## State (2026-11-25)

Checked on one machine: CachyOS (kernel 7.3), Ryzen 7 5700X, Radeon RX 7900
GRE on RADV (Mesa 26.3.1), with Proton 11.0-3, Proton Experimental and
Proton-CachyOS 11.0-20261015.

| What | State |
|------|-------|
| Build on Linux | works, without Microsoft's SDK (`tools/linux/cross-env.sh`) |
| `cargo test --workspace` under Proton | passes: the loader's stand-in title is mapped, bound and called; the translated shaders of the tests run on the card and give the reference values |
| `victory warm` over a title's archives | completes; see "Pipelines that end the process" for the ones left out |
| The title itself (`victory run --execute`) | **not run.** The dump at hand had no executable. Everything a run needs before the title's first instruction is covered by the tests above; what the title then does to Wine's memory manager, its threads and vkd3d-proton at full load is not known |

So: it builds, starts, loads and makes its pipelines on Linux. Whether the
world draws as on Windows, and how fast, is for whoever runs it first with
a whole dump to say.

## Building

Needs rustup, `lld`, `llvm`, `clang`, and Wine with its import libraries
(`/usr/lib/wine/x86_64-windows/lib*.a`; the distribution's `wine` package).

```sh
. tools/linux/cross-env.sh
cargo build --release -p victory --target x86_64-pc-windows-msvc
```

The target is the one Windows builds use. Instead of Microsoft's import
libraries and C runtime startup code, the link takes Wine's: Wine ships an
import library for every DLL it implements, and `lld-link` reads them. The
script links them under the names the Rust target asks for and adds the
few startup symbols Wine's code leaves to the toolchain. Nothing of
Microsoft's is downloaded. `cargo xwin` (which fetches Microsoft's SDK)
should do as well; it was not tried.

## Running

Under Proton's wine, outside Steam:

```sh
export VICTORY_GAME_DIR='Z:\path\to\your\dump'     # Z: is /
tools/linux/proton-wine.sh target/x86_64-pc-windows-msvc/release/victory.exe config
tools/linux/proton-wine.sh target/x86_64-pc-windows-msvc/release/victory.exe warm
tools/linux/proton-wine.sh target/x86_64-pc-windows-msvc/release/victory.exe run \
    'Z:\path\to\your\dump\<the title's exe>' --execute
```

The script takes Proton Experimental if Steam has it, else the newest
Proton it finds where Steam keeps them (`PROTON=<its folder>` names one), has it make a prefix of its own under
`~/.local/share/victory/proton`, and starts Proton's `wine` with the
prefix's vkd3d-proton and DXVK DXGI. Paths are Windows paths: `Z:\` is the
root of the Linux file system. The cache and the saves are in the prefix
(`C:\users\steamuser\AppData\Local\victory`) unless `VICTORY_CACHE_DIR` and
`VICTORY_DATA_DIR` say otherwise.

Plain Wine starts `victory.exe` too, but its own Direct3D 12 (vkd3d) has no
shader model 6: nothing of a title would draw. vkd3d-proton is needed.

**Run `victory warm` first** (see the next section). Under Wine it starts
its work again by itself each time a shader ends it, and says how many
pipelines it left out when it is done.

## Pipelines that end the process

On Windows the driver takes our DXIL as it is. Under Wine it goes through
vkd3d-proton's conversion to SPIR-V (dxil-spirv), which has to find
structured control flow in it, and then through the Vulkan driver's
compiler, all inside our process. For some of the title's shaders the
conversion writes SPIR-V in which a block leaves a loop by neither its merge
nor its continue, and RADV then faults inside `vkCreateComputePipelines`;
for others the conversion recurses until the stack is gone, however large.
Neither is an error returned to us: the process ends (or hangs), and a run
that asks for the same pipeline at the same place ends the same way.

Two things are done about it.

- **The shape of the code** (`victory-shader`, `emit/bitcode/exits.rs`): a
  translated program returns from one block, and each of its loops is left
  towards one block. That is the shape the DXIL compiler's own output has
  and the conversion is used to. Of the title's first 100 compute programs
  7 ended the process without it, none with it. It is on under Wine only,
  where it is needed (Windows drivers are given the code they were given
  so far): one more bit of `VICTORY_SHADER_OPT` (64), and so part of the
  shader cache's key.
- **The quarantine** (`victory-gpu`, `pipeline/quarantine.rs`): the cache
  keeps a journal of the pipelines being made. A run that finds pipelines
  begun and not done makes each of them alone; one that was begun alone and
  not done is written to `pipelines/refused.txt` in the cache and left out
  from then on, its draws dropped like those of a shader that does not
  translate. Delete a line of that file to have its pipeline tried again
  (after an update of Proton or Mesa, say).

`victory warm` makes every compute pipeline and, under Wine, every vertex,
pixel and mesh shader alone with stand-ins for the other stages, so that
what ends the process is found there and not in the world's first frames.
Under Wine the work is done by a second process that `warm` starts again
whenever it ends early or makes no pipeline for five minutes
(`VICTORY_WARM_HANG_S`): at most two starts per bad shader. `warm --once`
is one such start. (`VICTORY_WARM_STAGES` and `VICTORY_WARM_AGAIN`, `0` or
`1`, turn the two on or off on either system.)

On the machine above, with the title's 1.0.2.1 archives (25,688 shaders
translate), one `victory warm` from an empty cache:

| Proton | Compute pipelines made | Other shaders made alone | Left out by the quarantine |
|--------|------------------------|--------------------------|----------------------------|
| Experimental 11.0-20261112 | 1,723 | 22,961, none refused by the host | 3 compute programs, in each of which the conversion does not return |
| Proton-CachyOS 11.0-20261015 | not finished when this was written | not finished | 26 pixel shaders (RADV faults on the SPIR-V) and 2 compute programs so far |

**Which vkd3d-proton it is matters more than anything here**: use the
newest. With Experimental's, `warm` took about 45 minutes, nearly all of it
waiting ten minutes at a time for the three programs that hang (the wait is
five minutes now); a second `warm` takes 45 seconds.

`CS_ScreenProbes_ActivateRays` (the title's largest compute program) is
left out under Wine without being tried: the conversion had not finished it
after 40 minutes. `VICTORY_ACTIVATE_RAYS=1` tries it.

## What else differs

- **Our own HLSL** is compiled at run time by `d3dcompiler_47.dll`, which
  under Wine is Wine's own compiler: it knows no method of a byte-address
  buffer and not the `RootSignature` attribute. Our shaders use neither now
  (structured buffers, root signatures made in code) and build with both
  compilers. Microsoft's DLL beside `victory.exe` is used instead if it is
  there; the log says whose compiler is in use.
- **An empty pipeline library** is asked for with a null blob. The windows
  crate's wrapper passed a non-null pointer with length 0, which
  vkd3d-proton refuses: no library, and no pipeline kept between runs.
- **vkd3d-proton's own pipeline cache** is put in the cache folder
  (`VKD3D_SHADER_CACHE_PATH`, unless set): its default is the working
  directory, which may be the repository.
- **DXIL hashes** are not checked by vkd3d-proton; a test that expects a
  wrong one to be refused skips that part under Wine.
- `VICTORY_INPLACE` (title memory opened as a graphics heap) does not work:
  its probe fails under vkd3d-proton at the committed section. It is off by
  default.
- There is no software adapter (WARP): `VICTORY_D3D12_WARP` finds none, and
  the tests that want one skip themselves.

## Tests

```sh
. tools/linux/cross-env.sh           # VICTORY_TEST_WINE=tools/linux/proton-wine.sh for Proton
cargo build --manifest-path tests/fixtures/hle-probe/Cargo.toml \
    --target-dir target/hle-probe --target x86_64-pc-windows-msvc
export VICTORY_TEST_PROBE_DLL='Z:\...\target\hle-probe\x86_64-pc-windows-msvc\debug\hle_probe.dll'
cargo test --workspace --target x86_64-pc-windows-msvc
cargo test -p xtask                  # the asset-policy check, on the host
```

Test executables run under Wine cannot start the host's `cargo` or `git`:
the loader's stand-in title is built beforehand and named in
`VICTORY_TEST_PROBE_DLL`, and `xtask`'s policy test is run natively. With
plain Wine as the runner the tests that need shader model 6.6 fail for want
of a device; with Proton they run on the card.

Some tests take the repository's path from the build (a Linux path). Wine
reads it on the current drive: keep the prefix without a drive mapped to a
folder above the repository other than `Z:` (Proton's launcher adds none
when it is run as the script runs it; `umu-run` maps `X:` to the home
folder, and those tests then fail to find the repository).

## Not looked at

- Frame times. The Windows numbers in the README are not Linux numbers.
- Input (XInput under Wine), audio (XAudio2 under Wine), the window and
  presentation through DXVK's DXGI.
- NVIDIA and Intel cards; any Mesa but 26.3.

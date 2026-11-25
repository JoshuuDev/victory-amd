//! Builds the title's helper shaders in `src/shaders/` ahead with `dxc`, when
//! there is one (the `DXC` environment variable, else `dxc` on the `PATH`).
//!
//! The bytecode goes to `OUT_DIR` and is embedded with `include_bytes!`;
//! nothing compiled enters the repository. Without `dxc` the files are
//! empty and the shaders are compiled at first use with `D3DCompile`, as
//! before. Wine's `D3DCompile` (vkd3d-shader) builds none of them, so a
//! build for Wine or Proton wants `dxc`.

use std::path::PathBuf;
use std::process::Command;

/// File, target profile. Every entry point is `main`; a source that
/// defines `RS` carries its root signature (`-rootsig-define RS`).
const SHADERS: &[(&str, &str)] = &[
    ("clamp", "cs_6_0"),
    ("card-compare", "cs_6_0"),
    ("plane-corner", "vs_6_0"),
    ("plane-steps", "ps_6_0"),
    ("plane-floats", "ps_6_0"),
];

fn main() {
    println!("cargo:rerun-if-env-changed=DXC");
    println!("cargo:rerun-if-env-changed=PATH");
    println!("cargo:rerun-if-changed=build.rs");
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    let dxc = std::env::var_os("DXC").unwrap_or_else(|| "dxc".into());
    let have = Command::new(&dxc)
        .arg("-help")
        .output()
        .is_ok_and(|o| o.status.success());
    for (name, profile) in SHADERS {
        let source = format!("src/shaders/{name}.hlsl");
        println!("cargo:rerun-if-changed={source}");
        let target = out.join(format!("{name}.dxil"));
        let _ = std::fs::remove_file(&target);
        let mut built = false;
        if have {
            let text = std::fs::read_to_string(&source).expect("shader source");
            let mut cmd = Command::new(&dxc);
            cmd.args(["-nologo", "-T", profile, "-E", "main"]);
            if text.contains("#define RS") {
                cmd.args(["-rootsig-define", "RS"]);
            }
            let result = cmd.arg("-Fo").arg(&target).arg(&source).output();
            match result {
                Ok(o) if o.status.success() => built = true,
                Ok(o) => println!(
                    "cargo:warning=dxc failed on {source}: {}",
                    String::from_utf8_lossy(&o.stderr).replace('\n', " ")
                ),
                Err(e) => println!("cargo:warning=dxc did not run on {source}: {e}"),
            }
        }
        if !built {
            std::fs::write(&target, []).expect("OUT_DIR");
        }
    }
}

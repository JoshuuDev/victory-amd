//! The helper shaders of `src/shaders/` as `build.rs` built them with `dxc`
//! (empty without one: the callers then have only `D3DCompile`).

macro_rules! prebuilt {
    ($name:ident, $file:literal) => {
        pub(crate) const $name: &[u8] =
            include_bytes!(concat!(env!("OUT_DIR"), "/", $file, ".dxil"));
    };
}

prebuilt!(CLAMP, "clamp");
prebuilt!(CARD_COMPARE, "card-compare");
prebuilt!(PLANE_CORNER, "plane-corner");
prebuilt!(PLANE_STEPS, "plane-steps");
prebuilt!(PLANE_FLOATS, "plane-floats");

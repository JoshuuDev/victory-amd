//! Wine as the host (Proton included): Linux runs VIctory as the Windows
//! program it is, with vkd3d-proton as its Direct3D 12. What differs there
//! from Windows, and is ours to mind, is kept here. See docs/linux.md.

use std::path::Path;
use std::sync::OnceLock;

use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress, LoadLibraryA};

/// Wine's version if the host is Wine, as its ntdll gives it.
pub fn version() -> Option<&'static str> {
    static VERSION: OnceLock<Option<String>> = OnceLock::new();
    VERSION
        .get_or_init(|| {
            // SAFETY: ntdll is in every process; the export, where it
            // exists, takes nothing and returns a static C string.
            unsafe {
                let ntdll = GetModuleHandleA(c"ntdll.dll".as_ptr().cast());
                let get = GetProcAddress(ntdll, c"wine_get_version".as_ptr().cast())?;
                let get: unsafe extern "C" fn() -> *const std::ffi::c_char = std::mem::transmute(get);
                let version = get();
                (!version.is_null()).then(|| std::ffi::CStr::from_ptr(version).to_string_lossy().into_owned())
            }
        })
        .as_deref()
}

/// Whether the HLSL compiler our own shaders are built with at run time
/// (`D3DCompile`, d3dcompiler_47.dll) is Wine's own, and not Microsoft's
/// DLL beside victory.exe or installed in the prefix, which is loaded
/// instead of Wine's where it is. Wine's does not know the methods of
/// byte-address buffers nor the `RootSignature` attribute, so our shaders
/// use neither: their buffers are structured ones and their root signatures
/// are made in code (`crate::host::root_signature`), and either compiler
/// builds them.
pub fn compiler_is_builtin() -> bool {
    static BUILTIN: OnceLock<bool> = OnceLock::new();
    *BUILTIN.get_or_init(|| {
        if version().is_none() {
            return false;
        }
        // SAFETY: loading a system DLL we import anyway; a module handle is
        // its image's base, whose DOS header is at least 0x80 bytes.
        unsafe {
            let module = LoadLibraryA(c"d3dcompiler_47.dll".as_ptr().cast());
            !module.is_null() && is_builtin(std::slice::from_raw_parts(module.cast::<u8>(), 0x80))
        }
    })
}

/// Wine marks the DLLs it implements itself in the DOS header, where a
/// Windows image has its stub program.
fn is_builtin(image: &[u8]) -> bool {
    const MARK: &[u8] = b"Wine builtin DLL";
    image.get(0x40..0x40 + MARK.len()) == Some(MARK)
}

/// Logs what the host is, once, and whose compiler builds our shaders.
pub(crate) fn report() {
    let Some(version) = version() else { return };
    tracing::info!(version, "the host is Wine");
    let compiler = if compiler_is_builtin() { "Wine's own" } else { "Microsoft's" };
    tracing::info!(compiler, "d3dcompiler_47.dll, which builds our own shaders");
}

/// vkd3d-proton keeps its own cache of compiled pipelines, by default in a
/// file in the working directory: made from the title's shaders, it belongs
/// with the rest of what is derived from the title. It goes to `dir`
/// unless the user has said where it goes (`VKD3D_SHADER_CACHE_PATH`).
/// To be called before the host device is made.
pub(crate) fn keep_host_cache_in(dir: &Path) {
    const VAR: &str = "VKD3D_SHADER_CACHE_PATH";
    if version().is_none() || std::env::var_os(VAR).is_some() {
        return;
    }
    if std::fs::create_dir_all(dir).is_ok() {
        // SAFETY: called while the run is set up, before the threads that
        // read the environment (the host device's) exist.
        unsafe { std::env::set_var(VAR, dir) };
        tracing::info!(dir = %dir.display(), "vkd3d-proton's pipeline cache is kept with ours");
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_mark_of_a_builtin_dll_is_read_from_its_dos_header() {
        let mut image = vec![0u8; 0x80];
        image[..2].copy_from_slice(b"MZ");
        assert!(!super::is_builtin(&image));
        image[0x40..0x50].copy_from_slice(b"Wine builtin DLL");
        assert!(super::is_builtin(&image));
        assert!(!super::is_builtin(&image[..0x48]));
    }

    /// On Windows there is no Wine and nothing is reported as its own.
    #[test]
    fn a_compiler_is_wine_s_only_on_wine() {
        if super::version().is_none() {
            assert!(!super::compiler_is_builtin());
        }
    }
}

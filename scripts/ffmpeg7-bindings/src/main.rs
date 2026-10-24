//! Preparation only: generate Rust declarations from original public headers.
//! No decoder, media input, C compiler, linker against FFmpeg, or application UI.
use serde_json::{json, Value};
use std::{collections::BTreeSet, error::Error, fs, path::{Path, PathBuf}, sync::{Arc, Mutex}};

type Result<T> = std::result::Result<T, Box<dyn Error>>;
const TARGET: &str = "x86_64-pc-windows-msvc";
const TYPES: &[&str] = &[
    "AVCodecContext", "AVCodec", "AVCodecHWConfig", "AVCodecParameters",
    "AVFrame", "AVPacket", "AVBufferRef", "AVHWDeviceContext", "AVHWFramesContext",
    "AVD3D11VADeviceContext", "AVD3D11VAFramesContext", "AVD3D11FrameDescriptor",
    "AVFormatContext", "AVStream", "AVRational", "AVIOInterruptCB",
    "AVPixelFormat", "AVHWDeviceType", "AVColorSpace", "AVColorRange",
    "AVColorPrimaries", "AVColorTransferCharacteristic", "AVFieldOrder",
    // Opaque pointees must be explicitly emitted: bindgen does not necessarily
    // recursively allowlist opaque definitions referenced by selected structs.
    "ID3D11Device", "ID3D11DeviceContext", "ID3D11VideoDevice",
    "ID3D11VideoContext", "ID3D11Texture2D",
];
#[derive(Debug)]
struct IncludedFiles(Arc<Mutex<BTreeSet<String>>>);
impl bindgen::callbacks::ParseCallbacks for IncludedFiles {
    fn include_file(&self, filename: &str) {
        self.0.lock().unwrap().insert(filename.to_owned());
    }
}

fn absolute(value: &Value, name: &str) -> Result<PathBuf> {
    let path = PathBuf::from(value[name].as_str().ok_or("Missing absolute path")?);
    if !path.is_absolute() { return Err(format!("{name} must be absolute").into()); }
    Ok(path.canonicalize()?)
}

#[cfg(windows)]
unsafe fn runtime_versions(directory: &Path) -> Result<Value> {
    use libloading::os::windows::{Library, LOAD_LIBRARY_SEARCH_DEFAULT_DIRS, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR};
    let mut versions = serde_json::Map::new();
    let mut keep_alive = Vec::new();
    for (file, function, expected) in [
        ("avutil-59.dll", "avutil_version", (59, 8, 100)),
        ("avcodec-61.dll", "avcodec_version", (61, 3, 100)),
        ("avformat-61.dll", "avformat_version", (61, 1, 100)),
        ("swscale-8.dll", "swscale_version", (8, 1, 100)),
    ] {
        let path = directory.join(file).canonicalize()?;
        let library = Library::load_with_flags(&path,
            LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS)?;
        let version = library.get::<unsafe extern "C" fn() -> u32>(function.as_bytes())?();
        let actual = (version >> 16, (version >> 8) & 255, version & 255);
        if actual != expected { return Err(format!("{} returned {actual:?}, expected {expected:?}", path.display()).into()); }
        versions.insert(file.into(), json!({ "path": path, "version": actual, "integer": version }));
        keep_alive.push(library);
    }
    // No FFmpeg structure is accessed; libraries remain live through all calls.
    Ok(Value::Object(versions))
}

fn main() -> Result<()> {
    if !cfg!(all(windows, target_arch = "x86_64", target_env = "msvc")) {
        return Err("Run this preparation helper on Windows x86-64 MSVC".into());
    }
    let config_path = PathBuf::from(std::env::args_os().nth(1).ok_or("Expected configuration JSON path")?);
    let config: Value = serde_json::from_slice(&fs::read(config_path)?)?;
    let wrapper = absolute(&config, "wrapper")?;
    let output = absolute(&config, "output")?;
    let dlls = absolute(&config, "runtime_directory")?;
    let libclang = absolute(&config, "libclang_dll")?;
    let expected_libclang_directory = libclang.parent().ok_or("No libclang directory")?;
    let configured = std::env::var_os("LIBCLANG_PATH").ok_or("LIBCLANG_PATH must be explicit")?;
    if PathBuf::from(configured).canonicalize()? != expected_libclang_directory {
        return Err("LIBCLANG_PATH differs from the reviewed absolute DLL directory".into());
    }
    // Do not let developer-specific environment flags silently change ABI input.
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("BINDGEN_EXTRA_CLANG_ARGS") {
            return Err(format!("Clear {} before deterministic generation", name.to_string_lossy()).into());
        }
    }
    #[cfg(windows)]
    let versions = unsafe { runtime_versions(&dlls)? };
    #[cfg(not(windows))]
    let versions = Value::Null;
    #[cfg(windows)]
    let _clang_loaded = unsafe {
        use libloading::os::windows::*;
        Library::load_with_flags(&libclang,
            LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS)?
    };
    let clang = bindgen::clang_version();
    if clang.parsed != Some((18, 1)) { return Err(format!("Unexpected libclang: {}", clang.full).into()); }
    let clang_args: Vec<String> = config["clang_args"].as_array().ok_or("Missing clang_args")?
        .iter().map(|arg| arg.as_str().map(str::to_owned).ok_or("Non-string clang argument"))
        .collect::<std::result::Result<_, _>>()?;
    if !clang_args.iter().any(|arg| arg == &format!("--target={TARGET}")) {
        return Err("Missing exact Windows x86-64 target".into());
    }
    let included_files = Arc::new(Mutex::new(BTreeSet::new()));
    let mut builder = bindgen::Builder::default()
        .header(wrapper.to_str().ok_or("Non-UTF8 wrapper path")?)
        .clang_args(&clang_args)
        .detect_include_paths(false)
        .layout_tests(true)
        .generate_comments(false)
        .derive_debug(false)
        .derive_default(false)
        .generate_inline_functions(false)
        .formatter(bindgen::Formatter::Rustfmt)
        .allowlist_recursively(true)
        .parse_callbacks(Box::new(IncludedFiles(included_files.clone())))
        .allowlist_var("AV_.*|LIBAV.*|FF_API_.*")
        .blocklist_function(".*")
        // Parse real SDK declarations, but expose COM pointees as opaque Rust
        // types. Production interfaces use windows-rs, not bindgen's COM ABI.
        .opaque_type("I(D3D11|DXGI|Unknown).*|_GUID|GUID|IUnknown")
        .raw_line("// Original FFmpeg 7.0.2 public-header ABI; generated, never hand-padded.");
    for name in TYPES { builder = builder.allowlist_type(name); }
    let bindings = builder.generate().map_err(|_| "bindgen failed; no offsets were fabricated")?;
    let text = bindings.to_string();
    let compact: String = text.chars().filter(|ch| !ch.is_whitespace()).collect();
    for required in ["pubhw_device_ctx:", "pubhw_frames_ctx:", "pubget_format:", "pubdevice:", "pubdata:"] {
        if !compact.contains(required) { return Err(format!("Generated declarations lack {required}").into()); }
    }
    fs::write(output.join("bindings.rs"), text)?;
    fs::write(output.join("generation.json"), serde_json::to_vec_pretty(&json!({
        "schema": 1, "target": TARGET, "bindgen": "0.71.1", "libclang": clang.full,
        "runtime_versions": versions, "wrapper": wrapper, "clang_args": clang_args,
        "allowlisted_types": TYPES, "opaque_com": true, "layout_tests": true,
        "included_files": &*included_files.lock().unwrap(),
        "formatter": "rustfmt", "compiled_c_bridge": false,
        "input_record": config["input_record"], "production_integration": false,
    }))?)?;
    println!("Generated {}", output.join("bindings.rs").display());
    Ok(())
}

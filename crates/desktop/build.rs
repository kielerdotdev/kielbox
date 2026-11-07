use std::{env, fs, path::PathBuf};

fn resource_text(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"").replace(['\r', '\n'], " ")
}

fn main() {
    println!("cargo:rerun-if-env-changed=KIELBOX_BUILD_VERSION");
    println!("cargo:rerun-if-changed=../../assets/kielbox.ico");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let package_version = env::var("CARGO_PKG_VERSION").unwrap();
    let display_version = env::var("KIELBOX_BUILD_VERSION").unwrap_or_else(|_| package_version.clone());
    let numbers: Vec<u16> = package_version.split('.').map(|part| part.parse().unwrap_or(0)).collect();
    let major = numbers.first().copied().unwrap_or(0);
    let minor = numbers.get(1).copied().unwrap_or(0);
    let patch = numbers.get(2).copied().unwrap_or(0);
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let icon = resource_text(&root.join("../../assets/kielbox.ico").to_string_lossy());
    let version = resource_text(&display_version);
    let content = format!(r#"#pragma code_page(65001)
1 ICON "{icon}"
1 VERSIONINFO
FILEVERSION {major},{minor},{patch},0
PRODUCTVERSION {major},{minor},{patch},0
FILEFLAGSMASK 0x3fL
FILEFLAGS 0
FILEOS 0x40004L
FILETYPE 1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "FileDescription", "Kielbox\0"
      VALUE "FileVersion", "{version}\0"
      VALUE "InternalName", "Kielbox\0"
      VALUE "OriginalFilename", "Kielbox.exe\0"
      VALUE "ProductName", "Kielbox\0"
      VALUE "ProductVersion", "{version}\0"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x0409, 1200
  END
END
"#);
    let resource = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("kielbox.rc");
    fs::write(&resource, content).expect("Write Kielbox executable resources");
    embed_resource::compile(&resource, embed_resource::NONE)
        .manifest_required()
        .expect("Compile Kielbox executable resources");
}

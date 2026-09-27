// Embeds the icon, manifest and version info into the exe.
use std::path::Path;

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = std::env::var("OUT_DIR").unwrap();
    let ver = env!("CARGO_PKG_VERSION");
    let mut n: Vec<u32> = ver.split(['.', '-']).filter_map(|p| p.parse().ok()).collect();
    n.resize(4, 0);
    // rc.exe wants forward slashes or doubled backslashes in paths.
    let esc = |p: &Path| p.display().to_string().replace('\\', "/");
    let rc = format!(
        r#"#pragma code_page(65001)
1 ICON "{icon}"
1 24 "{manifest}"
1 VERSIONINFO
FILEVERSION {a},{b},{c},{d}
PRODUCTVERSION {a},{b},{c},{d}
FILEOS 0x40004
FILETYPE 0x1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "FileDescription", "Apple Music Discord Presence"
      VALUE "ProductName", "Apple Music Discord Presence"
      VALUE "FileVersion", "{ver}"
      VALUE "ProductVersion", "{ver}"
      VALUE "OriginalFilename", "AppleMusicDiscordPresence.exe"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#,
        icon = esc(&root.join("assets/icon.ico")),
        manifest = esc(&root.join("res/app.manifest")),
        a = n[0],
        b = n[1],
        c = n[2],
        d = n[3],
    );
    let rc_path = Path::new(&out).join("app.rc");
    std::fs::write(&rc_path, rc).unwrap();
    embed_resource::compile(&rc_path, embed_resource::NONE).manifest_required().unwrap();
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-changed=res/app.manifest");

    // The app links no C runtime at all (src/rt.rs supplies the few pieces
    // it needs); `rawentry` is the process entry point. .pdata (unwind
    // tables) shares .rdata's padding: Windows finds it via the PE header.
    for arg in ["/NODEFAULTLIB", "/ENTRY:rawentry", "/DEBUG:NONE", "/NOCOFFGRPINFO", "/MERGE:.pdata=.rdata"] {
        println!("cargo:rustc-link-arg-bins={arg}");
    }
}

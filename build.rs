fn main() {
    #[cfg(windows)]
    {
        // Embed the longPathAware manifest so that Win32 APIs accept paths > 260 chars.
        println!("cargo:rerun-if-changed=fpt.manifest");
        embed_manifest();
    }
}

#[cfg(windows)]
fn embed_manifest() {
    // Use the winres crate-style approach: compile the manifest as a resource.
    // This is simpler than depending on winres; we just invoke mt.exe or rc.exe
    // if available.  Falling back silently means long paths need \\?\ prefix instead.
    //
    // A simpler approach: just set the linker flag that tells Windows we are
    // long-path aware.  This works on Windows 10 1607+ with the registry key
    // HKLM\SYSTEM\CurrentControlSet\Control\FileSystem\LongPathsEnabled = 1.
    //
    // We embed the manifest via a resource file.
    use std::path::Path;
    use std::process::Command;

    let manifest = Path::new("fpt.manifest");
    if !manifest.exists() {
        return;
    }

    // Create a .rc file that references the manifest
    let rc_content = format!(
        "1 24 \"{}\"",
        manifest.canonicalize().unwrap_or(manifest.to_path_buf())
            .to_string_lossy()
            .replace('\\', "\\\\")
    );
    let rc_path = Path::new(&std::env::var("OUT_DIR").unwrap()).join("fpt.rc");
    std::fs::write(&rc_path, rc_content).ok();

    // Try to compile the resource with rc.exe (Windows SDK)
    let out_dir = std::env::var("OUT_DIR").unwrap();
    let res_path = Path::new(&out_dir).join("fpt.res");

    let status = Command::new("rc.exe")
        .arg(&format!("/fo{}", res_path.display()))
        .arg(&rc_path)
        .status();

    if let Ok(s) = status {
        if s.success() {
            println!("cargo:rustc-link-arg-bins={}", res_path.display());
            return;
        }
    }

    // Fallback: just print a warning. The process will still work for
    // paths < MAX_PATH, and Win32 calls we control can use \\?\ prefix.
    println!("cargo:warning=Could not embed longPathAware manifest. Long paths may be limited to 260 chars.");
}

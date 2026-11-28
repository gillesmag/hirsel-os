use std::env;
use std::fs;
use std::path::Path;

fn main() {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let core_dir = Path::new(&manifest_dir).join("../../../hirsel/core/core");
    let os_file = Path::new(&manifest_dir).join("../../hirsel-os-lib/os.hirsel");

    println!("cargo:rerun-if-changed={}", core_dir.display());
    println!("cargo:rerun-if-changed={}", os_file.display());

    let mut entries: Vec<(String, String)> = Vec::new();
    for entry in fs::read_dir(&core_dir).expect("read hirsel/core/core") {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("hirsel") {
            let stem = path.file_stem().unwrap().to_string_lossy().to_string();
            let abs = fs::canonicalize(&path).unwrap();
            println!("cargo:rerun-if-changed={}", abs.display());
            entries.push((stem, abs.to_string_lossy().to_string()));
        }
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let os_abs = fs::canonicalize(&os_file).unwrap();
    let mut out = String::new();
    out.push_str("pub const EMBEDDED_CORE: &[(&str, &str)] = &[\n");
    for (stem, abs) in &entries {
        out.push_str(&format!("    ({stem:?}, include_str!({abs:?})),\n"));
    }
    out.push_str("];\n");
    out.push_str(&format!(
        "pub const EMBEDDED_OS_HIRSEL: &str = include_str!({:?});\n",
        os_abs.to_string_lossy()
    ));

    let out_dir = env::var("OUT_DIR").unwrap();
    fs::write(Path::new(&out_dir).join("embedded_libs.rs"), out).unwrap();
}

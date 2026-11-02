use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};
use toml_edit::DocumentMut;

const MAX_FILES: usize = 4096;
const MAX_MANIFESTS: usize = 256;
const MAX_TOTAL: usize = 8 * 1024 * 1024;
fn walk(root: &Path, depth: usize, files: &mut Vec<PathBuf>, seen: &mut usize) {
    assert!(depth <= 8, "plugin bundle depth exceeded");
    let mut entries = Vec::new();
    for entry in fs::read_dir(root).expect("read plugin bundle directory") {
        *seen += 1;
        assert!(*seen <= MAX_FILES, "plugin bundle file limit exceeded");
        entries.push(entry.expect("read plugin entry").path());
    }
    entries.sort();
    for path in entries {
        let meta = fs::symlink_metadata(&path).expect("inspect bundle entry");
        assert!(!meta.is_symlink(), "bundle symlinks are unsupported");
        if meta.is_dir() {
            walk(&path, depth + 1, files, seen);
        } else if path.file_name().is_some_and(|n| n == "plugin.toml") {
            assert!(meta.is_file(), "manifest must be a regular file");
            assert!(
                files.len() < MAX_MANIFESTS,
                "plugin bundle manifest limit exceeded"
            );
            files.push(path);
        }
    }
}
fn read(path: &Path, limit: usize, total: &mut usize) -> String {
    let meta = fs::symlink_metadata(path).expect("inspect bundle resource");
    assert!(
        meta.is_file() && !meta.is_symlink() && meta.len() <= limit as u64,
        "unsafe or oversized bundle resource"
    );
    let mut text = String::new();
    fs::File::open(path)
        .expect("open bundle resource")
        .take(limit as u64 + 1)
        .read_to_string(&mut text)
        .expect("read UTF-8 bundle resource");
    assert!(text.len() <= limit, "oversized bundle resource");
    *total += text.len();
    assert!(*total <= MAX_TOTAL, "plugin bundle total size exceeded");
    println!("cargo:rerun-if-changed={}", path.display());
    text
}
fn main() {
    let root = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("crate directory"))
        .join("../../plugins");
    println!("cargo:rerun-if-changed={}", root.display());
    let mut files = Vec::new();
    walk(&root, 0, &mut files, &mut 0);
    files.sort();
    let mut output = String::from("static BUNDLE: &[BundleEntry] = &[\n");
    let mut total = 0;
    for path in files {
        let label = path
            .strip_prefix(&root)
            .expect("bundle relative path")
            .to_str()
            .expect("UTF-8 bundle path")
            .replace('\\', "/");
        let text = read(&path, 256 * 1024, &mut total);
        let document: DocumentMut = text.parse().expect("valid bundled TOML");
        let mut resources = Vec::new();
        if let Some(agents) = document
            .get("contributions")
            .and_then(|v| v.get("agents"))
            .and_then(|v| v.as_array_of_tables())
        {
            assert!(agents.len() <= 64, "too many bundled Agent definitions");
            for agent in agents {
                if agent.get("engine").and_then(|v| v.as_str()) != Some("declarative-config/1") {
                    continue;
                }
                let name = agent
                    .get("definition")
                    .and_then(|v| v.as_str())
                    .expect("Agent definition path");
                // Adjacent resources keep source-package ownership unambiguous.
                assert!(
                    !name.is_empty()
                        && name.len() <= 128
                        && Path::new(name).components().count() == 1
                        && matches!(
                            Path::new(name).components().next(),
                            Some(std::path::Component::Normal(_))
                        ),
                    "definition must be an adjacent resource"
                );
                let body = read(
                    &path.parent().expect("package directory").join(name),
                    64 * 1024,
                    &mut total,
                );
                resources.push((name.to_owned(), body));
            }
        }
        // Debug formatting emits escaped Rust string literals, never executable source from TOML.
        output.push_str(&format!(
            "BundleEntry {{ label: {label:?}, text: {text:?}, resources: &["
        ));
        for (name, body) in resources {
            output.push_str(&format!("({name:?}, {body:?}),"));
        }
        output.push_str("] },\n");
    }
    output.push_str("];\n");
    fs::write(
        PathBuf::from(std::env::var_os("OUT_DIR").expect("build output directory"))
            .join("bundle.rs"),
        output,
    )
    .expect("write bundle module");
}

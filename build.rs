use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn collect(root: &Path, directory: &Path, files: &mut Vec<PathBuf>) {
    let mut entries = fs::read_dir(directory)
        .unwrap_or_else(|error| {
            panic!(
                "read frontend asset directory {}: {error}",
                directory.display()
            )
        })
        .map(|entry| entry.expect("read frontend asset entry").path())
        .collect::<Vec<_>>();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            collect(root, &path, files);
        } else if path.is_file() {
            files.push(
                path.strip_prefix(root)
                    .expect("frontend asset is below root")
                    .to_path_buf(),
            );
        }
    }
}

fn source_identity(frontend: &Path) -> String {
    let mut files = [
        "index.html",
        "package.json",
        "deno.json",
        "deno.lock",
        "tsconfig.json",
        "vite.config.ts",
    ]
    .into_iter()
    .map(PathBuf::from)
    .collect::<Vec<_>>();
    collect(frontend, &frontend.join("src"), &mut files);
    for file in &mut files {
        if file.is_absolute() {
            *file = file
                .strip_prefix(frontend)
                .expect("frontend source is rooted")
                .to_path_buf();
        }
    }
    files.sort();
    let mut identity = 0xcbf29ce484222325_u64;
    for relative in files {
        for byte in relative.to_string_lossy().bytes().chain(
            fs::read(frontend.join(&relative))
                .unwrap_or_else(|error| {
                    panic!("read frontend source {}: {error}", relative.display())
                })
                .into_iter(),
        ) {
            identity ^= byte as u64;
            identity = identity.wrapping_mul(0x100000001b3);
        }
    }
    format!("fnv1a64-{identity:016x}")
}

fn content_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
    {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

fn main() {
    println!("cargo:rerun-if-env-changed=AGENTICJIRA_FRONTEND_DIST");
    println!("cargo:rerun-if-env-changed=AGENTICJIRA_ALLOW_FOUNDATION");
    println!("cargo:rerun-if-changed=resources/foundation.html");
    println!("cargo:rerun-if-changed=resources/trip-explorer/0.12.0");
    println!("cargo:rerun-if-changed=resources/workflows/trip-explorer-0.12.0-llmrelay-1.json");
    println!("cargo:rerun-if-changed=resources/prompts/trip-overlay.md");
    println!("cargo:rerun-if-changed=frontend/dist");
    println!("cargo:rerun-if-changed=frontend/src");
    for file in [
        "frontend/index.html",
        "frontend/package.json",
        "frontend/deno.json",
        "frontend/deno.lock",
        "frontend/tsconfig.json",
        "frontend/vite.config.ts",
    ] {
        println!("cargo:rerun-if-changed={file}");
    }
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is set"));
    let configured = env::var_os("AGENTICJIRA_FRONTEND_DIST").map(PathBuf::from);
    let built = configured.unwrap_or_else(|| PathBuf::from("frontend/dist"));
    let (root, files) = if built.join("index.html").is_file() {
        let expected = source_identity(Path::new("frontend"));
        let recorded = fs::read_to_string(built.join(".agenticjira-source-id")).unwrap_or_default();
        if recorded.trim() != expected {
            panic!(
                "LLMRelay dashboard assets at {} are stale for frontend source {}. Run the rooted frontend build before Cargo.",
                built.display(), expected
            )
        }
        let root = built.canonicalize().expect("resolve frontend dist");
        let mut files = Vec::new();
        collect(&root, &root, &mut files);
        files.retain(|path| path != Path::new(".agenticjira-source-id"));
        (root, files)
    } else if env::var_os("AGENTICJIRA_ALLOW_FOUNDATION").as_deref()
        == Some(std::ffi::OsStr::new("1"))
    {
        let file = PathBuf::from("resources/foundation.html")
            .canonicalize()
            .expect("resolve fallback dashboard");
        (
            file.parent().expect("fallback parent").to_path_buf(),
            vec![PathBuf::from("foundation.html")],
        )
    } else {
        panic!(
            "LLMRelay dashboard assets are missing at {}. Run the rooted frontend build first, or set AGENTICJIRA_ALLOW_FOUNDATION=1 only for an explicit recovery build.",
            built.display()
        )
    };
    let mut source = String::from("pub const ASSETS: &[(&str, &[u8], &str)] = &[\n");
    let mut asset_identity = 0xcbf29ce484222325_u64;
    for relative in files {
        let absolute = root.join(&relative);
        for byte in relative.to_string_lossy().bytes().chain(
            fs::read(&absolute)
                .expect("read embedded frontend asset")
                .into_iter(),
        ) {
            asset_identity ^= byte as u64;
            asset_identity = asset_identity.wrapping_mul(0x100000001b3);
        }
        let route = if relative.file_name().and_then(|value| value.to_str()) == Some("index.html") {
            "/".to_owned()
        } else if relative.file_name().and_then(|value| value.to_str()) == Some("foundation.html") {
            "/".to_owned()
        } else {
            format!("/{}", relative.to_string_lossy().replace('\\', "/"))
        };
        source.push_str(&format!(
            "    ({route:?}, include_bytes!({:?}), {:?}),\n",
            absolute,
            content_type(&relative)
        ));
    }
    source.push_str("];\n");
    source.push_str(&format!(
        "pub const ASSET_IDENTITY: &str = \"fnv1a64-{asset_identity:016x}\";\n"
    ));
    fs::write(output.join("embedded_assets.rs"), source).expect("write embedded frontend registry");
}

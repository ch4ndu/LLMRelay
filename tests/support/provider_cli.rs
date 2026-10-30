use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// Supplies only executable/version observations. All provider execution is refused;
/// host contracts using this fixture are not native-provider qualification.
pub fn install_codex_version_probe(root: &Path) {
    let bin = root.join("provider-version-probes");
    std::fs::create_dir_all(&bin).unwrap();
    let codex = bin.join("codex");
    std::fs::write(
        &codex,
        format!(
            "#!/bin/sh\nif [ \"$#\" -eq 1 ] && [ \"$1\" = \"--version\" ]; then\n  printf '%s\\n' '{}'\n  exit 0\nfi\nprintf '%s\\n' 'Host-test fixture refuses provider execution.' >&2\nexit 64\n",
            agenticjira::providers::codex::EXACT_CODEX_VERSION,
        ),
    )
    .unwrap();
    std::fs::set_permissions(&codex, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut paths = vec![bin];
    if let Some(current) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&current));
    }
    std::env::set_var("PATH", std::env::join_paths(paths).unwrap());
}

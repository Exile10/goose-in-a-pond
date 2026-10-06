//! GIAP's suggestions are shown and never imposed: only the REST view may read them. Seeding,
//! startup, activation and downloads deciding anything from `recommended` would choose a model
//! on the household's behalf. Parses source, so it sees a reader in a crate CI doesn't test.

use std::path::{Path, PathBuf};

/// The module itself, the DTO types and the row builder.
const READERS: &[&str] = &[
    "crates/pond-core/src/models/domain/recommended.rs",
    "crates/pond-api/src/lib.rs",
    "crates/pond-api/src/model_views.rs",
];

const NEEDLES: &[&str] = &["recommended::", "recommendation_for(", "RECOMMENDED"];

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("CARGO_MANIFEST_DIR has two ancestors")
        .to_path_buf()
}

fn collect_rs(dir: &Path, root: &Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if path.is_dir() {
            // Integration tests may inspect the suggestions; they choose nothing.
            if name == "target" || name == "tests" || name.starts_with('.') {
                continue;
            }
            collect_rs(&path, root, out);
        } else if name.ends_with(".rs") {
            out.push(
                path.strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
        }
    }
}

fn without_line_comments(src: &str) -> String {
    src.lines()
        .map(|l| match l.find("//") {
            Some(i) => &l[..i],
            None => l,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn only_the_rest_view_reads_the_recommendations() {
    let root = workspace_root();
    let mut files = Vec::new();
    collect_rs(&root.join("crates"), &root, &mut files);
    assert!(
        files.len() > 300,
        "scanned only {} files; the walk has broken",
        files.len()
    );

    let mut readers: Vec<String> = files
        .iter()
        .filter(|rel| {
            let src = std::fs::read_to_string(root.join(rel)).unwrap_or_default();
            let code = without_line_comments(&src);
            NEEDLES.iter().any(|n| code.contains(n))
        })
        .cloned()
        .collect();
    readers.sort();

    let unexpected: Vec<&String> = readers
        .iter()
        .filter(|f| !READERS.contains(&f.as_str()))
        .collect();
    assert!(
        unexpected.is_empty(),
        "these files read GIAP's recommendations: {unexpected:?}. A suggestion is shown to the \
         household and never acted on; choose nothing from it outside the REST view."
    );
    for reader in READERS {
        assert!(
            readers.iter().any(|r| r == reader),
            "{reader} no longer reads the recommendations; this guard's list is stale"
        );
    }
}

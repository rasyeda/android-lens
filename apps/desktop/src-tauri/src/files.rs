use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

const IGNORED_DIRS: &[&str] = &[
    ".git",
    ".gradle",
    ".idea",
    ".kotlin",
    "build",
    "node_modules",
    "target",
    ".cxx",
    ".dart_tool",
];

const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Debug, Clone, Serialize)]
pub struct FsEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceFileContent {
    pub path: String,
    pub file_name: String,
    pub content: String,
    pub language: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectFile {
    pub name: String,
    pub rel_path: String,
    pub abs_path: String,
}

const QUICK_OPEN_EXTS: &[&str] = &[
    "kt",
    "kts",
    "java",
    "xml",
    "gradle",
    "json",
    "toml",
    "properties",
    "md",
];

pub fn list_project_files(project_root: &str) -> Result<Vec<ProjectFile>, String> {
    let root = Path::new(project_root);
    if !root.is_dir() {
        return Err(format!("'{project_root}' is not a directory"));
    }
    let mut out = Vec::new();
    collect_quick_open_files(root, root, 0, &mut out);
    out.sort_by_key(|f| f.name.to_lowercase());
    Ok(out)
}

fn collect_quick_open_files(root: &Path, dir: &Path, depth: usize, out: &mut Vec<ProjectFile>) {
    if depth > 14 || out.len() >= 2000 {
        return;
    }
    let Ok(read) = fs::read_dir(dir) else {
        return;
    };
    for entry in read.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            if !IGNORED_DIRS.contains(&name.as_str()) {
                collect_quick_open_files(root, &path, depth + 1, out);
            }
        } else {
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if QUICK_OPEN_EXTS.contains(&ext.as_str()) {
                let rel_path = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push(ProjectFile {
                    name,
                    rel_path,
                    abs_path: path.to_string_lossy().to_string(),
                });
            }
        }
    }
}

pub fn list_dir(dir: &str) -> Result<Vec<FsEntry>, String> {
    let path = Path::new(dir);
    let read = fs::read_dir(path).map_err(|e| format!("failed to read '{dir}': {e}"))?;
    let mut dirs = Vec::new();
    let mut files = Vec::new();

    for entry in read.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') && name != ".env" {
            continue;
        }
        let p = entry.path();
        let is_dir = p.is_dir();
        if is_dir && IGNORED_DIRS.contains(&name.as_str()) {
            continue;
        }
        let item = FsEntry {
            name,
            path: p.to_string_lossy().to_string(),
            is_dir,
        };
        if is_dir {
            dirs.push(item);
        } else {
            files.push(item);
        }
    }

    dirs.sort_by_key(|a| a.name.to_lowercase());
    files.sort_by_key(|a| a.name.to_lowercase());
    dirs.extend(files);
    Ok(dirs)
}

pub fn read_source_file(path_str: &str) -> Result<SourceFileContent, String> {
    let path = Path::new(path_str);
    let meta = fs::metadata(path).map_err(|e| format!("cannot stat '{path_str}': {e}"))?;
    if meta.len() > MAX_FILE_BYTES {
        return Err(format!(
            "file exceeds 2 MB viewer limit ({} bytes)",
            meta.len()
        ));
    }
    let content = fs::read_to_string(path).map_err(|e| format!("cannot read '{path_str}': {e}"))?;
    let file_name = path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| path_str.to_string());
    let language = match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "kt" | "kts" => "kotlin",
        "java" => "java",
        _ => "text",
    }
    .to_string();

    Ok(SourceFileContent {
        path: path.to_string_lossy().to_string(),
        file_name,
        content,
        language,
    })
}

/// Fast recursive lookup mapping a JDWP `SourceFile` name (e.g. `MainActivity.kt`)
/// to a file inside the user's open project folder.
pub fn find_source_file(
    project_root: &str,
    file_name: &str,
    class_hint: Option<&str>,
) -> Option<String> {
    let mut matches: Vec<PathBuf> = Vec::new();
    collect_matching_files(Path::new(project_root), file_name, 0, &mut matches);
    if matches.is_empty() {
        return None;
    }
    if matches.len() == 1 {
        return Some(matches[0].to_string_lossy().to_string());
    }

    // Disambiguate using the JVM class package path (e.g. `com/everybit/sholattv`)
    if let Some(cls) = class_hint {
        let pkg_path = cls
            .rsplit_once('.')
            .map(|(pkg, _)| pkg.replace('.', "/"))
            .unwrap_or_default();
        if !pkg_path.is_empty() {
            if let Some(best) = matches
                .iter()
                .find(|p| p.to_string_lossy().replace('\\', "/").contains(&pkg_path))
            {
                return Some(best.to_string_lossy().to_string());
            }
        }
    }

    // Prefer `src/main` over worktrees/tests
    if let Some(best) = matches.iter().find(|p| {
        let s = p.to_string_lossy().replace('\\', "/");
        s.contains("/src/main/") && !s.contains("/.claude/")
    }) {
        return Some(best.to_string_lossy().to_string());
    }

    Some(matches[0].to_string_lossy().to_string())
}

fn collect_matching_files(dir: &Path, target_name: &str, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > 14 || out.len() >= 16 {
        return;
    }
    let Ok(read) = fs::read_dir(dir) else {
        return;
    };
    for entry in read.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            if !IGNORED_DIRS.contains(&name.as_str()) {
                collect_matching_files(&path, target_name, depth + 1, out);
            }
        } else if name.eq_ignore_ascii_case(target_name) {
            out.push(path);
        }
    }
}

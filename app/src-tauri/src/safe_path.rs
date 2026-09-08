//! Safe path resolution shared by the export / import / data-directory commands.
//!
//! Responsibilities: lexical normalization, expansion of Windows 8.3 short
//! aliases through the longest existing ancestor, component-boundary
//! containment checks and the protected-directory policy.

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

const NON_DISK_PREFIX: &str = "禁止使用网络共享、命名空间或非标准路径前缀";
const PARENT_DIR: &str = "路径包含非法目录遍历字符 (..)";
const NOT_ABSOLUTE: &str = "路径必须为绝对路径";
const NO_EXISTING_ANCESTOR: &str = "路径没有可解析的已存在祖先";

/// Lexically normalize `path`: drop `.`, reject `..`, reject non-disk prefixes
/// (UNC, `\\?\`, `\\.\`) on Windows and require an absolute result.
fn lexically_normalize(path: &Path) -> Result<PathBuf, String> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => {
                #[cfg(target_os = "windows")]
                if !matches!(prefix.kind(), std::path::Prefix::Disk(_)) {
                    return Err(NON_DISK_PREFIX.into());
                }
                #[cfg(not(target_os = "windows"))]
                let _ = prefix;
                normalized.push(component);
            }
            Component::RootDir => normalized.push(component),
            Component::CurDir => {}
            Component::ParentDir => return Err(PARENT_DIR.into()),
            Component::Normal(name) => normalized.push(name),
        }
    }
    if !normalized.is_absolute() {
        return Err(NOT_ABSOLUTE.into());
    }
    Ok(normalized)
}

/// Turn a `\\?\C:\...` verbatim disk path (as returned by `canonicalize`) back
/// into the plain `C:\...` form. Other prefixes are returned unchanged.
#[cfg(target_os = "windows")]
fn strip_verbatim_prefix(path: &Path) -> PathBuf {
    let mut components = path.components();
    let Some(Component::Prefix(prefix)) = components.next() else {
        return path.to_path_buf();
    };
    let std::path::Prefix::VerbatimDisk(letter) = prefix.kind() else {
        return path.to_path_buf();
    };
    let mut plain = PathBuf::from(format!("{}:\\", letter as char));
    for component in components {
        if let Component::Normal(name) = component {
            plain.push(name);
        }
    }
    plain
}

#[cfg(not(target_os = "windows"))]
fn strip_verbatim_prefix(path: &Path) -> PathBuf {
    path.to_path_buf()
}

/// Lexically normalize `path` (reject relative paths, `..`, UNC / device
/// namespaces), then walk up to the first existing ancestor, canonicalize it
/// and re-append the non-existent tail in its original order. On Windows the
/// `\\?\` verbatim disk prefix is turned back into a plain drive path. Fails
/// closed whenever no ancestor can be resolved.
pub(crate) fn resolve_for_validation(path: &Path) -> Result<PathBuf, String> {
    let normalized = lexically_normalize(path)?;
    let mut ancestor = normalized.as_path();
    let mut tail: Vec<OsString> = Vec::new();
    while !ancestor.exists() {
        let name = ancestor
            .file_name()
            .ok_or_else(|| NO_EXISTING_ANCESTOR.to_string())?;
        tail.push(name.to_os_string());
        ancestor = ancestor
            .parent()
            .ok_or_else(|| NO_EXISTING_ANCESTOR.to_string())?;
    }
    let canonical =
        std::fs::canonicalize(ancestor).map_err(|error| format!("无法解析目标路径：{error}"))?;
    let mut resolved = strip_verbatim_prefix(&canonical);
    for component in tail.into_iter().rev() {
        resolved.push(component);
    }
    Ok(resolved)
}

/// Case-folded comparison keys for every component of `path`. Disk and
/// verbatim-disk prefixes collapse to the same `c:` key so `\\?\C:\x` and
/// `c:\x` compare equal.
#[cfg(target_os = "windows")]
fn comparable_components(path: &Path) -> Vec<String> {
    path.components()
        .filter_map(|component| match component {
            Component::Prefix(prefix) => Some(match prefix.kind() {
                std::path::Prefix::Disk(letter) | std::path::Prefix::VerbatimDisk(letter) => {
                    format!("{}:", (letter as char).to_ascii_lowercase())
                }
                _ => prefix.as_os_str().to_string_lossy().to_lowercase(),
            }),
            Component::RootDir => Some(String::from("\\")),
            Component::CurDir => None,
            Component::ParentDir => Some(String::from("..")),
            Component::Normal(name) => Some(name.to_string_lossy().to_lowercase()),
        })
        .collect()
}

/// Component-boundary containment: `path` equals `root` or lives under it.
/// Windows compares components case-insensitively; other platforms use
/// `Path::starts_with`. An empty root never contains anything.
#[cfg(target_os = "windows")]
pub(crate) fn path_is_within(path: &Path, root: &Path) -> bool {
    let root_parts = comparable_components(root);
    if root_parts.is_empty() {
        return false;
    }
    let path_parts = comparable_components(path);
    path_parts.len() >= root_parts.len()
        && path_parts
            .iter()
            .zip(root_parts.iter())
            .all(|(actual, expected)| actual == expected)
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn path_is_within(path: &Path, root: &Path) -> bool {
    !root.as_os_str().is_empty() && path.starts_with(root)
}

/// Resolve a protected/allowed root through the same pipeline as the target.
/// Returns `None` when the root is unset or does not exist (nothing to compare
/// against); a root that exists but cannot be canonicalized falls back to its
/// lexical normalization so the rule still applies.
fn resolve_root(candidate: Option<String>) -> Option<PathBuf> {
    let candidate = candidate?;
    let candidate = candidate.trim();
    if candidate.is_empty() {
        return None;
    }
    let root = Path::new(candidate);
    if !root.exists() {
        return None;
    }
    resolve_for_validation(root)
        .or_else(|_| lexically_normalize(root))
        .ok()
}

fn env_root(name: &str) -> Option<PathBuf> {
    resolve_root(std::env::var(name).ok())
}

/// Built-in protected roots were always applied lexically; keep that as the
/// floor when the directory happens not to exist on this machine.
fn builtin_root(literal: &str) -> Option<PathBuf> {
    resolve_root(Some(literal.to_string())).or_else(|| Some(PathBuf::from(literal)))
}

fn is_within_temp(resolved: &Path) -> bool {
    [
        env_root("TEMP"),
        env_root("TMP"),
        resolve_root(Some(std::env::temp_dir().to_string_lossy().into_owned())),
    ]
    .into_iter()
    .flatten()
    .any(|temp| path_is_within(resolved, &temp))
}

/// `true` when `root` resolved to something and `resolved` lives inside it.
fn within_root(resolved: &Path, root: Option<PathBuf>) -> bool {
    root.is_some_and(|root| path_is_within(resolved, &root))
}

fn enforce_protected_directories(resolved: &Path) -> Result<(), String> {
    let forbidden_roots = [
        "c:\\windows",
        "c:\\program files",
        "c:\\program files (x86)",
        "c:\\programdata",
    ];
    for forbidden in forbidden_roots {
        if within_root(resolved, builtin_root(forbidden)) {
            return Err(format!("禁止访问系统关键目录：{forbidden}"));
        }
    }

    if within_root(resolved, env_root("WINDIR")) {
        return Err("禁止访问系统 Windows 目录".into());
    }
    if within_root(resolved, env_root("APPDATA")) {
        return Err("禁止访问用户 AppData 目录".into());
    }
    if within_root(resolved, env_root("LOCALAPPDATA")) {
        return Err("禁止访问用户 LocalAppData 目录".into());
    }

    if let Some(userprofile) = env_root("USERPROFILE") {
        let in_profile = path_is_within(resolved, &userprofile);
        if !in_profile && within_root(resolved, builtin_root("c:\\users")) {
            return Err("禁止访问非当前登录用户的目录".into());
        }
        if in_profile {
            let first_segment = resolved
                .components()
                .skip(userprofile.components().count())
                .find_map(|component| match component {
                    Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
                    _ => None,
                });
            if let Some(first_segment) = first_segment {
                if first_segment.starts_with('.') {
                    return Err(format!("禁止访问用户配置目录 ({first_segment})"));
                }
                if first_segment.eq_ignore_ascii_case("appdata") {
                    return Err("禁止访问 AppData 目录".into());
                }
            }
        }
    }

    let touches_startup = resolved.components().any(|component| match component {
        Component::Normal(name) => {
            let name = name.to_string_lossy();
            name.eq_ignore_ascii_case("startup") || name.eq_ignore_ascii_case("start menu")
        }
        _ => false,
    });
    if touches_startup {
        return Err("禁止访问系统启动或开始菜单目录".into());
    }
    Ok(())
}

/// Resolve `path` and enforce the protected-directory policy (TEMP exception,
/// Windows, Program Files, ProgramData, AppData, other users' profiles,
/// Startup / Start Menu). Returns the resolved path.
pub(crate) fn validate_writable_destination(path: &Path) -> Result<PathBuf, String> {
    let resolved = resolve_for_validation(path)?;
    if is_within_temp(&resolved) {
        return Ok(resolved);
    }
    enforce_protected_directories(&resolved)?;
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_dir(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("{label}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn has_short_alias(path: &Path) -> bool {
        path.components()
            .any(|component| component.as_os_str().to_string_lossy().contains('~'))
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn component_boundary_does_not_match_sibling_prefix() {
        assert!(!path_is_within(
            Path::new(r"C:\Temporary\x"),
            Path::new(r"C:\Temp")
        ));
        assert!(!path_is_within(
            Path::new(r"C:\Temp_Evil\x"),
            Path::new(r"C:\Temp")
        ));
        assert!(path_is_within(
            Path::new(r"C:\Temp\x"),
            Path::new(r"C:\Temp")
        ));
        assert!(path_is_within(Path::new(r"C:\Temp"), Path::new(r"C:\Temp")));
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn component_boundary_does_not_match_sibling_prefix() {
        assert!(!path_is_within(Path::new("/tmp_evil/x"), Path::new("/tmp")));
        assert!(!path_is_within(Path::new("/tmporary/x"), Path::new("/tmp")));
        assert!(path_is_within(Path::new("/tmp/x"), Path::new("/tmp")));
        assert!(path_is_within(Path::new("/tmp"), Path::new("/tmp")));
        assert!(!path_is_within(Path::new("/tmp/x"), Path::new("")));
    }

    #[test]
    fn missing_or_empty_root_is_skipped() {
        let missing = std::env::temp_dir()
            .join(format!("acm-missing-root-{}", uuid::Uuid::new_v4()))
            .to_string_lossy()
            .into_owned();
        assert!(resolve_root(Some(missing)).is_none());
        assert!(resolve_root(Some(String::from("  "))).is_none());
        assert!(resolve_root(None).is_none());
        let existing =
            resolve_root(Some(std::env::temp_dir().to_string_lossy().into_owned())).unwrap();
        assert!(!existing.to_string_lossy().starts_with(r"\\?\"));
    }

    #[test]
    fn preserves_nonexistent_tail_after_resolving_existing_ancestor() {
        let root = scratch_dir("acm-safe-path");
        let result = resolve_for_validation(&root.join("new/child/file.md")).unwrap();
        assert!(result.ends_with(Path::new("new/child/file.md")));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_relative_and_traversal_paths() {
        assert!(resolve_for_validation(Path::new("relative/file.md")).is_err());
        assert!(resolve_for_validation(&std::env::temp_dir().join("..").join("x.md")).is_err());
    }

    #[test]
    fn temp_children_are_writable_and_returned_resolved() {
        let root = scratch_dir("acm-safe-path-temp");
        let target = root.join("nested").join("out.md");
        let resolved = validate_writable_destination(&target).unwrap();
        assert!(resolved.ends_with(Path::new("nested/out.md")));
        assert!(!resolved.to_string_lossy().starts_with(r"\\?\"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn rejects_non_disk_prefixes() {
        assert!(resolve_for_validation(Path::new(r"\\evil.com\share\doc.md")).is_err());
        assert!(resolve_for_validation(Path::new(r"\\?\C:\Windows\evil.md")).is_err());
        assert!(resolve_for_validation(Path::new(r"\\.\COM1")).is_err());
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn fails_closed_when_no_ancestor_exists() {
        let Some(missing_drive) = ('D'..='Z')
            .map(|letter| format!("{letter}:\\"))
            .find(|drive| !Path::new(drive).exists())
        else {
            eprintln!("skip: every drive letter exists, cannot build a path without ancestor");
            return;
        };
        let target = Path::new(&missing_drive).join("dir").join("file.md");
        assert!(resolve_for_validation(&target).is_err());
        assert!(validate_writable_destination(&target).is_err());
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn path_is_within_ignores_case_and_verbatim_prefix() {
        assert!(path_is_within(
            Path::new(r"\\?\C:\TEMP\x"),
            Path::new(r"c:\temp")
        ));
        assert!(path_is_within(
            Path::new(r"C:\Users\Administrator\AppData"),
            Path::new(r"c:\users\administrator\")
        ));
        assert!(!path_is_within(
            Path::new(r"D:\Temp\x"),
            Path::new(r"C:\Temp")
        ));
        assert!(!path_is_within(Path::new(r"C:\Temp\x"), Path::new("")));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn expands_8_3_short_alias_in_temp_dir() {
        let temp = std::env::temp_dir();
        if !has_short_alias(&temp) {
            eprintln!(
                "skip: temp dir {} carries no 8.3 alias on this machine",
                temp.display()
            );
            return;
        }
        let resolved = resolve_for_validation(&temp.join("acm-alias").join("file.md")).unwrap();
        assert!(
            !has_short_alias(&resolved),
            "resolved path still carries an 8.3 alias: {}",
            resolved.display()
        );
        assert!(!resolved.to_string_lossy().starts_with(r"\\?\"));
        let canonical_temp = std::fs::canonicalize(&temp).unwrap();
        assert!(path_is_within(&resolved, &canonical_temp));
        assert!(resolved.ends_with(Path::new("acm-alias/file.md")));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn short_alias_into_program_files_is_rejected() {
        let alias = Path::new(r"C:\PROGRA~1");
        if !alias.exists() {
            eprintln!("skip: C:\\PROGRA~1 alias is not available on this machine");
            return;
        }
        let target = alias.join("acm-evil").join("test.md");
        assert!(validate_writable_destination(&target).is_err());
        assert!(validate_writable_destination(Path::new(r"C:\Program Files\acm\test.md")).is_err());
        assert!(validate_writable_destination(Path::new(r"C:\Windows\System32\evil.md")).is_err());
        assert!(validate_writable_destination(Path::new(r"C:\ProgramData\test.md")).is_err());
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn temp_sibling_prefix_is_not_treated_as_temp() {
        let temp = std::fs::canonicalize(std::env::temp_dir()).unwrap();
        let Some(parent) = temp.parent() else {
            eprintln!("skip: temp dir has no parent");
            return;
        };
        let name = temp.file_name().unwrap().to_string_lossy().into_owned();
        let sibling = parent.join(format!("{name}_Evil")).join("x.md");
        if sibling.parent().unwrap().exists() {
            eprintln!("skip: {} already exists", sibling.display());
            return;
        }
        assert!(
            !path_is_within(&sibling, &temp),
            "{} must not count as inside {}",
            sibling.display(),
            temp.display()
        );
        if path_is_within(&sibling, Path::new(r"C:\Users")) {
            // A sibling of %TEMP% under LocalAppData must not borrow the TEMP exception.
            assert!(validate_writable_destination(&sibling).is_err());
        }
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn startup_is_matched_per_component() {
        assert!(validate_writable_destination(Path::new(r"C:\acm-safe\Startup\x.md")).is_err());
        assert!(validate_writable_destination(Path::new(r"C:\acm-safe\Start Menu\x.md")).is_err());
        assert!(validate_writable_destination(Path::new(r"C:\acm-safe\StartupLogs\x.md")).is_ok());
    }
}

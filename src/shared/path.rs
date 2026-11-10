use std::path::Path;

/// Join a directory path and file name with `/` separators, so playlist
/// paths in recordings are identical on every operating system.
pub(crate) fn join_path(dir: &Path, file_name: &str) -> String {
    let mut path = String::new();
    for component in dir.components() {
        path.push_str(component.as_os_str().to_str().unwrap());
        path.push('/');
    }
    path.push_str(file_name);
    path
}

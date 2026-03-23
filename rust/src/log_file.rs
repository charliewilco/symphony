use std::path::{Path, PathBuf};

pub fn default_log_file() -> PathBuf {
    default_log_file_for_root(&std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

pub fn default_log_file_for_root(logs_root: &Path) -> PathBuf {
    logs_root.join("log").join("symphony.log")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_log_file_uses_current_working_directory() {
        assert_eq!(
            default_log_file(),
            std::env::current_dir()
                .unwrap()
                .join("log")
                .join("symphony.log")
        );
    }

    #[test]
    fn default_log_file_for_root_builds_path_under_custom_root() {
        assert_eq!(
            default_log_file_for_root(Path::new("/tmp/symphony-logs")),
            PathBuf::from("/tmp/symphony-logs/log/symphony.log")
        );
    }
}

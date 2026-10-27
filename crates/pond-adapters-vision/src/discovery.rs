//! Automatic discovery of local video capture devices (USB webcams, V4L2 nodes).

use std::path::{Path, PathBuf};

/// A local video device discovered via sysfs / V4L2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredCamera {
    /// Device node path, e.g. `/dev/video0`.
    pub path: PathBuf,
    /// Friendly device name, e.g. `"HD Pro Webcam C920"`.
    pub name: String,
    /// Device index from sysfs (primary video capture is index 0).
    pub index: u32,
}

/// Scan `/sys/class/video4linux/` for local video devices.
pub fn detect_v4l2_cameras() -> Vec<DiscoveredCamera> {
    scan_v4l2_sysfs(Path::new("/sys/class/video4linux"))
}

fn scan_v4l2_sysfs(sysfs: &Path) -> Vec<DiscoveredCamera> {
    let Ok(entries) = std::fs::read_dir(sysfs) else {
        return Vec::new();
    };

    let mut cameras = Vec::new();
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let name_str = file_name.to_string_lossy();
        if !name_str.starts_with("video") {
            continue;
        }

        let dev_path = PathBuf::from("/dev").join(&*name_str);
        let name = std::fs::read_to_string(entry.path().join("name"))
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|_| name_str.to_string());

        let index = std::fs::read_to_string(entry.path().join("index"))
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok())
            .unwrap_or(0);

        cameras.push(DiscoveredCamera {
            path: dev_path,
            name,
            index,
        });
    }

    cameras.sort_by(|a, b| a.path.cmp(&b.path));
    cameras
}

/// Find the default local video camera (primary video capture device with index 0).
pub fn default_camera_source() -> Option<PathBuf> {
    let cameras = detect_v4l2_cameras();
    cameras
        .into_iter()
        .find(|c| c.index == 0 && c.path.exists())
        .map(|c| c.path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn scan_v4l2_sysfs_discovers_cameras_and_parses_index() {
        let tmp = tempdir().unwrap();
        let sysfs = tmp.path();

        let v0 = sysfs.join("video0");
        std::fs::create_dir_all(&v0).unwrap();
        std::fs::write(v0.join("name"), "HD Pro Webcam C920\n").unwrap();
        std::fs::write(v0.join("index"), "0\n").unwrap();

        let v1 = sysfs.join("video1");
        std::fs::create_dir_all(&v1).unwrap();
        std::fs::write(v1.join("name"), "HD Pro Webcam C920\n").unwrap();
        std::fs::write(v1.join("index"), "1\n").unwrap();

        let cams = scan_v4l2_sysfs(sysfs);
        assert_eq!(cams.len(), 2);
        assert_eq!(cams[0].name, "HD Pro Webcam C920");
        assert_eq!(cams[0].index, 0);
        assert_eq!(cams[1].index, 1);
    }
}

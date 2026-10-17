//! rfkill state from sysfs, which any user may read.

use std::fs;
use std::path::Path;

const SYSFS_CLASS: &str = "/sys/class";

/// Whether rfkill blocks the adapter named `hci`, such as `hci0`.
pub(super) fn adapter_blocked(hci: &str) -> bool {
    adapter_blocked_in(Path::new(SYSFS_CLASS), hci)
}

/// Whether any Bluetooth rfkill switch is blocked.
pub(super) fn any_bluetooth_blocked() -> bool {
    any_bluetooth_blocked_in(Path::new(SYSFS_CLASS))
}

fn adapter_blocked_in(class: &Path, hci: &str) -> bool {
    let Ok(entries) = fs::read_dir(class.join("bluetooth").join(hci)) else {
        return false;
    };
    entries
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("rfkill"))
        .any(|entry| blocked(&entry.path()))
}

fn any_bluetooth_blocked_in(class: &Path) -> bool {
    let Ok(entries) = fs::read_dir(class.join("rfkill")) else {
        return false;
    };
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|switch| read(switch, "type").as_deref() == Some("bluetooth"))
        .any(|switch| blocked(&switch))
}

fn blocked(switch: &Path) -> bool {
    ["soft", "hard"]
        .into_iter()
        .any(|file| read(switch, file).as_deref() == Some("1"))
}

fn read(switch: &Path, file: &str) -> Option<String> {
    fs::read_to_string(switch.join(file))
        .ok()
        .map(|text| text.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/sysfs")
            .join(name)
    }

    #[test]
    fn a_soft_or_hard_block_blocks_the_adapter() {
        let class = fixture("blocked");
        assert!(adapter_blocked_in(&class, "hci0"));
        assert!(adapter_blocked_in(&class, "hci2"));
    }

    #[test]
    fn an_unblocked_or_switchless_adapter_is_not_blocked() {
        let class = fixture("blocked");
        assert!(!adapter_blocked_in(&class, "hci1"));
        assert!(!adapter_blocked_in(&class, "hci3"));
        assert!(!adapter_blocked_in(&class, "hci9"));
    }

    #[test]
    fn a_blocked_bluetooth_switch_counts_but_a_blocked_wlan_switch_does_not() {
        assert!(any_bluetooth_blocked_in(&fixture("blocked")));
        assert!(!any_bluetooth_blocked_in(&fixture("clear")));
        assert!(!any_bluetooth_blocked_in(&fixture("missing")));
    }
}

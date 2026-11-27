//! Whether a plugged-in security key is closed to this user. The `authenticator` library skips
//! such a key without a word, so the window would keep asking for a key that is already there.

use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io::{self, ErrorKind};
use std::path::Path;

/// The FIDO usage page (0xF1D0), as a HID report descriptor declares it.
const FIDO_USAGE_PAGE: [u8; 3] = [0x06, 0xd0, 0xf1];

/// A security key is plugged in, and this user may open none.
pub fn key_locked_out() -> bool {
    locked_out(Path::new("/sys/class/hidraw"), |name| {
        // As the library opens it; nothing is read or written.
        OpenOptions::new()
            .read(true)
            .write(true)
            .open(Path::new("/dev").join(name))
            .map(drop)
    })
}

/// `class` lists the HID devices, readable by everyone; `open` tries one of their nodes.
fn locked_out(class: &Path, open: impl Fn(&OsStr) -> io::Result<()>) -> bool {
    let Ok(devices) = fs::read_dir(class) else {
        return false;
    };
    let mut refused = false;
    for device in devices.flatten() {
        let is_key = fs::read(device.path().join("device/report_descriptor"))
            .is_ok_and(|bytes| bytes.windows(3).any(|item| item == FIDO_USAGE_PAGE));
        if !is_key {
            continue;
        }
        match open(&device.file_name()) {
            Ok(()) => return false,
            Err(e) => refused |= e.kind() == ErrorKind::PermissionDenied,
        }
    }
    refused
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    const KEYBOARD: &[u8] = &[0x05, 0x01, 0x09, 0x06];
    const SECURITY_KEY: &[u8] = &[0x06, 0xd0, 0xf1, 0x09, 0x01];

    /// A `/sys/class/hidraw` of its own, removed when dropped.
    struct Class(PathBuf);

    impl Class {
        fn new(devices: &[(&str, &[u8])]) -> Self {
            let root = std::env::temp_dir().join(format!("picsou-hidraw-{}", uuid::Uuid::new_v4()));
            for (name, descriptor) in devices {
                let device = root.join(name).join("device");
                fs::create_dir_all(&device).expect("device folder");
                fs::write(device.join("report_descriptor"), descriptor).expect("descriptor");
            }
            Self(root)
        }
    }

    impl Drop for Class {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn denied(_: &OsStr) -> io::Result<()> {
        Err(ErrorKind::PermissionDenied.into())
    }

    #[test]
    fn a_key_this_user_may_not_open_is_locked_out() {
        let class = Class::new(&[("hidraw0", KEYBOARD), ("hidraw1", SECURITY_KEY)]);

        assert!(locked_out(&class.0, denied));
    }

    #[test]
    fn one_key_that_opens_is_enough() {
        let class = Class::new(&[("hidraw0", SECURITY_KEY), ("hidraw1", SECURITY_KEY)]);

        assert!(!locked_out(&class.0, |name| match name.to_str() {
            Some("hidraw1") => Ok(()),
            _ => denied(name),
        }));
    }

    #[test]
    fn other_devices_and_other_errors_are_not_a_refusal() {
        let keyboard = Class::new(&[("hidraw0", KEYBOARD)]);
        let gone = Class::new(&[("hidraw0", SECURITY_KEY)]);

        assert!(!locked_out(&keyboard.0, denied));
        assert!(!locked_out(&gone.0, |_| Err(ErrorKind::NotFound.into())));
        assert!(!locked_out(&gone.0.join("missing"), denied));
    }
}

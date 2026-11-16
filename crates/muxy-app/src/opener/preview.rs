//! A single native Quick Look window; its private image lives only as long as the preview.
use std::io::{self, Write};
use std::process::{Child, Command, Stdio};

use muxy_protocol::{ChatImage, ChatImageType};

pub(crate) struct Preview {
    child: Child,
    _file: tempfile::NamedTempFile,
}

impl Preview {
    pub(crate) fn open(image: &ChatImage) -> io::Result<Self> {
        let suffix = match image.mime {
            ChatImageType::Png => ".png",
            ChatImageType::Jpeg => ".jpg",
            ChatImageType::Gif => ".gif",
            ChatImageType::Webp => ".webp",
        };
        let mut file = tempfile::Builder::new()
            .prefix("muxy-image-")
            .suffix(suffix)
            .tempfile()?;
        file.write_all(&image.bytes)?;
        let child = Command::new("/usr/bin/qlmanage")
            .arg("-p")
            .arg(file.path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        Ok(Self { child, _file: file })
    }

    pub(crate) fn finished(&mut self) -> io::Result<bool> {
        match self.child.try_wait()? {
            Some(status) if !status.success() => {
                Err(io::Error::other(format!("Quick Look exited with {status}")))
            }
            status => Ok(status.is_some()),
        }
    }
}

impl Drop for Preview {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replacing_a_preview_stops_its_process_and_removes_its_private_file() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let path = file.path().to_owned();
        let child = Command::new("/bin/sleep").arg("60").spawn().unwrap();
        let id = child.id();
        let preview = Preview { child, _file: file };
        drop(preview);
        assert!(!path.exists());
        assert!(
            !Command::new("/bin/kill")
                .args(["-0", &id.to_string()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .unwrap()
                .success()
        );
    }
}

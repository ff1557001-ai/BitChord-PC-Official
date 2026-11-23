use std::fs::{self, DirBuilder, File, OpenOptions, TryLockError};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::{fs::DirBuilderExt, net::UnixDatagram};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::{Link, SHOW, SHOW_MSG, parse, push};

const MAX_MESSAGE: usize = 4096;
const HANDOFF_TIMEOUT: Duration = Duration::from_secs(4);
const RELAUNCH_TIMEOUT: Duration = Duration::from_secs(8);

/// The lock elects the primary before eframe starts and survives until after
/// persistence and tray teardown. Only the primary can unlink the socket.
pub struct Instance {
    socket: UnixDatagram,
    path: PathBuf,
    _dir: File,
    _lock: File,
}

impl Instance {
    pub fn open(profile: &Path, link: Option<&Link>, relaunch: bool) -> io::Result<Option<Self>> {
        let dir = profile.join("instance");
        DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
        let dir = File::open(dir)?;
        // Linux resolves this name to the same socket file in both processes.
        // The short address works even when the profile exceeds sun_path.
        let root = PathBuf::from(format!("/proc/self/fd/{}", dir.as_raw_fd()));
        let lock = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(root.join("lock"))?;
        let path = root.join("socket");
        let start = Instant::now();
        let timeout = if relaunch {
            RELAUNCH_TIMEOUT
        } else {
            HANDOFF_TIMEOUT
        };
        loop {
            match lock.try_lock() {
                Ok(()) => {
                    match fs::remove_file(&path) {
                        Ok(()) => {}
                        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                        Err(err) => return Err(err),
                    }
                    return Ok(Some(Self {
                        socket: UnixDatagram::bind(&path)?,
                        path,
                        _dir: dir,
                        _lock: lock,
                    }));
                }
                Err(TryLockError::WouldBlock) => {
                    if !relaunch {
                        let message = link.map_or_else(|| SHOW_MSG.to_owned(), Link::url);
                        let client = UnixDatagram::unbound()?;
                        client.set_nonblocking(true)?;
                        if message.len() > MAX_MESSAGE {
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidInput,
                                "party link is too long",
                            ));
                        }
                        if client.send_to(message.as_bytes(), &path).is_ok() {
                            return Ok(None);
                        }
                    }
                    if start.elapsed() >= timeout {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "BitChord is running but could not receive the launch request",
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(TryLockError::Error(err)) => return Err(err),
            }
        }
    }

    pub fn listen(&self, wake: impl Fn() + Send + 'static) -> io::Result<()> {
        let socket = self.socket.try_clone()?;
        std::thread::Builder::new()
            .name("deeplink".into())
            .spawn(move || {
                let mut buffer = [0; MAX_MESSAGE + 1];
                loop {
                    let len = match socket.recv(&mut buffer) {
                        Ok(len) => len,
                        Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                        Err(err) => {
                            log::warn!("party link listener: {err}");
                            break;
                        }
                    };
                    if let Ok(message) = std::str::from_utf8(&buffer[..len])
                        && len <= MAX_MESSAGE
                        && receive(message)
                    {
                        wake();
                    }
                }
            })?;
        Ok(())
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn receive(message: &str) -> bool {
    if message == SHOW_MSG {
        SHOW.store(true, std::sync::atomic::Ordering::Relaxed);
        true
    } else if let Some(link) = parse(message) {
        push(link);
        true
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires permission to create UNIX-domain sockets"]
    fn launch_requests_reach_the_same_locked_profile() {
        let base = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("ipc-test-{}", std::process::id()));
        let profile = base.join("long-path".repeat(16));
        let first = Instance::open(&profile, None, false).unwrap().unwrap();
        first
            .socket
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        assert!(Instance::open(&profile, None, false).unwrap().is_none());
        let link = Link {
            code: "ABC234".into(),
            server: None,
        };
        assert!(
            Instance::open(&profile, Some(&link), false)
                .unwrap()
                .is_none()
        );
        let mut bytes = [0; MAX_MESSAGE];
        let n = first.socket.recv(&mut bytes).unwrap();
        assert_eq!(&bytes[..n], SHOW_MSG.as_bytes());
        let n = first.socket.recv(&mut bytes).unwrap();
        assert_eq!(parse(std::str::from_utf8(&bytes[..n]).unwrap()), Some(link));
        drop(first);
        assert!(Instance::open(&profile, None, false).unwrap().is_some());
        fs::remove_dir_all(base).unwrap();
    }
}

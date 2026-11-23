//! Linux app-owned helpers run in their own process groups so Quit also ends
//! WebKit and yt-dlp subprocesses. The lock covers spawn, reaping and signaling.

use std::io::{self, Read};
use std::os::unix::process::CommandExt;
use std::process::{Command, Output, Stdio};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

struct Children {
    stopping: bool,
    groups: Vec<u32>,
}

pub struct Helpers {
    children: Mutex<Children>,
    exited: Condvar,
}

pub static HELPERS: Helpers = Helpers::new();

impl Helpers {
    const fn new() -> Self {
        Self {
            children: Mutex::new(Children {
                stopping: false,
                groups: Vec::new(),
            }),
            exited: Condvar::new(),
        }
    }

    pub fn output(&self, command: &mut Command, timeout: Option<Duration>) -> io::Result<Output> {
        let mut children = self.children.lock().unwrap();
        if children.stopping {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "BitChord is closing",
            ));
        }
        let mut child = command.process_group(0).stdout(Stdio::piped()).spawn()?;
        let id = child.id();
        children.groups.push(id);
        drop(children);

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let started = Instant::now();
        std::thread::scope(|scope| {
            let drain = |mut pipe: Box<dyn Read + Send>| {
                let mut data = Vec::new();
                pipe.read_to_end(&mut data)?;
                io::Result::Ok(data)
            };
            let errors = stderr.map(|pipe| scope.spawn(move || drain(Box::new(pipe))));
            let result = stdout.map(|pipe| scope.spawn(move || drain(Box::new(pipe))));
            let mut timed_out = false;
            let status = loop {
                let mut children = self.children.lock().unwrap();
                // Keep the group ID reserved until its output pipes close. A
                // descendant can still hold a pipe after the direct child exits.
                let drained = result.as_ref().is_none_or(|reader| reader.is_finished())
                    && errors.as_ref().is_none_or(|reader| reader.is_finished());
                if drained && let Some(status) = child.try_wait().transpose() {
                    children.groups.retain(|&group| group != id);
                    self.exited.notify_all();
                    break status;
                }
                if !children.stopping
                    && !timed_out
                    && timeout.is_some_and(|limit| started.elapsed() >= limit)
                {
                    log::warn!("helper took too long; stopping process group {id}");
                    timed_out = true;
                    kill_group(id);
                }
                drop(children);
                std::thread::sleep(Duration::from_millis(25));
            }?;
            let stdout = result.map_or_else(|| Ok(Vec::new()), |reader| reader.join().unwrap())?;
            let stderr = errors.map_or_else(|| Ok(Vec::new()), |reader| reader.join().unwrap())?;
            Ok(Output {
                status,
                stdout,
                stderr,
            })
        })
    }

    pub fn stop(&self) {
        let mut children = self.children.lock().unwrap();
        children.stopping = true;
        for &id in &children.groups {
            kill_group(id);
        }
    }

    pub fn finish(&self) {
        self.stop();
        let children = self.children.lock().unwrap();
        let (children, _) = self
            .exited
            .wait_timeout_while(children, Duration::from_secs(2), |children| {
                !children.groups.is_empty()
            })
            .unwrap();
        if !children.groups.is_empty() {
            log::warn!("helper process shutdown timed out");
        }
    }
}

fn kill_group(id: u32) {
    // SAFETY: Every group was created by us and its direct child cannot be
    // reaped concurrently while the registry lock is held.
    unsafe {
        libc::killpg(id as libc::pid_t, libc::SIGKILL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;

    #[test]
    fn timeout_kills_and_reaps_the_owned_process_group() {
        // The shell and its background child both hold the output pipe open.
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "printf ready; sleep 30 & wait"]);
        let started = Instant::now();
        let helpers = Helpers::new();
        let output = helpers
            .output(&mut command, Some(Duration::from_millis(200)))
            .unwrap();
        assert_eq!(output.stdout, b"ready");
        assert_eq!(output.status.signal(), Some(libc::SIGKILL));
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(helpers.children.lock().unwrap().groups.is_empty());
    }

    #[test]
    fn shutdown_reaps_active_helpers_and_rejects_new_ones() {
        let helpers = Helpers::new();
        std::thread::scope(|scope| {
            let active = scope.spawn(|| {
                let mut command = Command::new("/bin/sh");
                command.args(["-c", "sleep 30 & wait"]);
                helpers.output(&mut command, None).unwrap()
            });
            let started = Instant::now();
            while helpers.children.lock().unwrap().groups.is_empty() {
                assert!(started.elapsed() < Duration::from_secs(2));
                std::thread::sleep(Duration::from_millis(5));
            }
            helpers.finish();
            let error = helpers
                .output(&mut Command::new("/bin/true"), None)
                .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::Interrupted);
            assert_eq!(active.join().unwrap().status.signal(), Some(libc::SIGKILL));
            assert!(helpers.children.lock().unwrap().groups.is_empty());
        });
    }
}

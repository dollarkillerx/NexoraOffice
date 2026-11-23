//! Writing the small state files on a thread of their own, so the window
//! never waits for the disk. Writes are atomic; when several writes to one
//! file are waiting, only the newest is made.

use std::io;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;

pub struct Store {
    sender: Option<Sender<(PathBuf, Vec<u8>)>>,
    thread: Option<JoinHandle<()>>,
}

impl Store {
    /// Start the thread. `failed` is called on it when a write fails.
    pub fn spawn(failed: impl Fn(PathBuf, io::Error) + Send + 'static) -> Self {
        let (sender, receiver) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("aster-workspace-store".into())
            .spawn(move || run(&receiver, &failed))
            .ok();
        Self {
            sender: Some(sender),
            thread,
        }
    }

    /// Replace `file` with `bytes`, soon. The folder is created if needed.
    pub fn write(&self, file: PathBuf, bytes: Vec<u8>) {
        if let Some(sender) = &self.sender {
            let _ = sender.send((file, bytes));
        }
    }

    /// Make the writes asked for so far and stop the thread.
    pub fn finish(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        self.sender = None;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run(receiver: &Receiver<(PathBuf, Vec<u8>)>, failed: &dyn Fn(PathBuf, io::Error)) {
    while let Ok(first) = receiver.recv() {
        // Whatever else is already waiting: the newest write per file wins.
        let mut waiting = vec![first];
        while let Ok(next) = receiver.try_recv() {
            match waiting.iter_mut().find(|(file, _)| *file == next.0) {
                Some(slot) => *slot = next,
                None => waiting.push(next),
            }
        }
        for (file, bytes) in waiting {
            let result = match file.parent() {
                Some(dir) => std::fs::create_dir_all(dir),
                None => Ok(()),
            }
            .and_then(|()| aster_storage::write_atomically(&file, &bytes));
            if let Err(err) = result {
                failed(file, err);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn the_newest_write_lands_and_folders_are_created() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("新しい").join("recent");
        let store = Store::spawn(|_, err| panic!("{err}"));
        for i in 0..100 {
            store.write(file.clone(), format!("version {i}").into_bytes());
        }
        store.finish();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "version 99");
    }

    #[test]
    fn a_failed_write_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        // A file where the folder should be.
        let blocker = dir.path().join("blocker");
        std::fs::write(&blocker, "x").unwrap();
        let failures = Arc::new(Mutex::new(Vec::new()));
        let seen = failures.clone();
        let store = Store::spawn(move |file, _| seen.lock().unwrap().push(file));
        store.write(blocker.join("recent"), b"x".to_vec());
        store.finish();
        assert_eq!(*failures.lock().unwrap(), [blocker.join("recent")]);
    }
}

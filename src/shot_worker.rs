//! The screenshot worker: the PNG encode and the clipboard write run
//! **off** the UI thread.
//!
//! F12 used to PNG-encode the whole frame — a few megabytes, tens of
//! milliseconds — and write the clipboard **synchronously on the event
//! loop**, so the parade visibly hiccuped on every shot
//! (audit 2026-10-22 §P2-13). The worker is the same shape as the
//! balloon, backdrop and asset loaders: a job channel in, an outcome
//! channel out, one thread that lives as long as the popup.
//!
//! The **snapshot** is still taken on the UI thread: the framebuffer is
//! blitted from the scene's own grid, and the scene keeps running while
//! the encode happens on a copy.

use agnosis_core::blit::FrameBuffer;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};

/// One screenshot job.
pub struct ShotJob {
    /// The frame to copy, already rendered.
    pub fb: FrameBuffer,
    /// Where the share PNG is written (`last_shot.png`).
    pub path: PathBuf,
}

/// The screenshot worker's handles.
pub struct ShotWorker {
    /// The jobs the popup hands over.
    jobs: Option<Sender<ShotJob>>,
    /// The outcomes the popup polls.
    outcomes: Receiver<Result<(), String>>,
}

impl ShotWorker {
    /// Start the worker thread.
    ///
    /// A thread that cannot be spawned leaves the worker inert: the shot
    /// reports a failure instead of freezing the parade.
    pub fn start() -> Self {
        let (job_tx, job_rx) = channel::<ShotJob>();
        let (out_tx, out_rx) = channel::<Result<(), String>>();
        // Detached by design: the thread lives as long as the popup, and
        // the loop ends when the job channel closes on drop. The handle
        // is deliberately not kept — the popup must not wait on the
        // encode at exit (audit 2026-10-23 §F-095).
        std::thread::spawn(move || {
            for job in job_rx {
                let outcome = crate::platform::clipboard::copy_frame(&job.fb)
                    .map_err(|e| e.to_string())
                    .and_then(|()| {
                        crate::platform::clipboard::write_png(&job.fb, &job.path)
                            .map_err(|e| e.to_string())
                    });
                if out_tx.send(outcome).is_err() {
                    // The popup is gone: nothing left to report to.
                    break;
                }
            }
        });
        Self {
            jobs: Some(job_tx),
            outcomes: out_rx,
        }
    }

    /// Hand one rendered frame to the worker.
    ///
    /// `false` when the worker is gone, so the caller can say so.
    pub fn send(&mut self, job: ShotJob) -> bool {
        let Some(jobs) = self.jobs.as_ref() else {
            return false;
        };
        if jobs.send(job).is_err() {
            self.jobs = None;
            return false;
        }
        true
    }

    /// Report the shots that finished, leaving the queue empty.
    pub fn drain(&self) -> std::sync::mpsc::TryIter<'_, Result<(), String>> {
        self.outcomes.try_iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny frame: 4 x 2 px, one inked pixel.
    fn tiny_frame() -> FrameBuffer {
        let mut fb = FrameBuffer::new(4, 2);
        fb.data[0] = 255;
        fb.data[2] = 255;
        fb
    }

    #[test]
    fn a_shot_comes_back_as_an_outcome() {
        let dir = std::env::temp_dir().join(format!(
            "agnosis-shot-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("last_shot.png");
        let mut worker = ShotWorker::start();
        assert!(
            worker.send(ShotJob {
                fb: tiny_frame(),
                path: path.clone(),
            }),
            "the worker takes the job"
        );
        // The outcome may take a moment — the encode and the clipboard
        // write are real work — so the poll sleeps between tries, inside
        // a bounded budget (the same discipline the platform waits use).
        let mut outcome = None;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            if let Some(one) = worker.drain().next() {
                outcome = Some(one);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        match outcome {
            Some(Ok(())) => {}
            Some(Err(e)) => {
                // A headless CI box has no clipboard; the file write is
                // still expected to succeed, so an error names itself.
                panic!("the shot failed, and the error names the cause: {e}");
            }
            None => panic!("the worker never reported the shot"),
        }
        assert!(path.is_file(), "the share PNG is written");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

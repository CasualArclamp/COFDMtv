//! The GUI's view of the transmitting engine: its latest snapshot, its messages, and a
//! history of what was sent.

use chrono::{DateTime, Local};
use cofdmtv_engine::{TxConfig, TxEvent, TxSnapshot, Transmitter};
use std::time::Instant;

/// One transmission (or a series of frames) sent.
#[derive(Debug, Clone)]
pub struct Sent {
    pub time: DateTime<Local>,
    pub what: String,
    pub seconds: f64,
    pub completed: bool,
}

#[derive(Default)]
pub struct TxSession {
    engine: Option<Transmitter>,
    stopping: bool,
    pub snap: TxSnapshot,
    /// What is on the air (for the status card and the history).
    pub label: String,
    pub error: Option<String>,
    /// Log lines of the transmitter.
    pub messages: Vec<String>,
    pub history: Vec<Sent>,
    started: Option<(Instant, DateTime<Local>)>,
    finished: bool,
}

impl TxSession {
    pub fn is_running(&self) -> bool {
        self.engine.is_some()
    }

    pub fn is_stopping(&self) -> bool {
        self.stopping
    }

    pub fn start(&mut self, cfg: TxConfig, label: String) {
        self.stop_now();
        self.label = label;
        self.error = None;
        self.finished = false;
        self.snap = TxSnapshot { running: true, jobs: cfg.jobs.len(), ..TxSnapshot::default() };
        self.started = Some((Instant::now(), Local::now()));
        self.engine = Some(Transmitter::start(cfg));
    }

    pub fn stop(&mut self) {
        if let Some(e) = &self.engine {
            e.stop();
            self.stopping = true;
        }
    }

    pub fn stop_now(&mut self) {
        if let Some(mut e) = self.engine.take() {
            e.stop();
            e.join();
        }
        self.stopping = false;
    }

    /// Seconds since the transmission started.
    pub fn elapsed(&self) -> Option<f64> {
        self.started.filter(|_| self.is_running()).map(|(s, _)| s.elapsed().as_secs_f64())
    }

    /// The transmission ended normally (everything sent).
    pub fn finished(&self) -> bool {
        self.finished
    }

    pub fn poll(&mut self) {
        let Some(engine) = &mut self.engine else { return };
        if let Some(s) = engine.snapshot_if_newer() {
            self.snap = s;
        }
        for ev in engine.poll_events() {
            match ev {
                TxEvent::Log(line) => self.messages.push(line),
                TxEvent::JobStarted { .. } => {}
                TxEvent::Finished => self.finished = true,
                TxEvent::Error(e) => {
                    self.messages.push(format!("error: {e}"));
                    self.error = Some(e);
                }
                TxEvent::Stopped => {
                    if let Some(mut e) = self.engine.take() {
                        e.join();
                    }
                    self.stopping = false;
                    self.snap.running = false;
                    if let Some((_, time)) = self.started.take() {
                        self.history.push(Sent { time, what: self.label.clone(), seconds: self.snap.seconds, completed: self.finished });
                    }
                }
            }
        }
        if self.messages.len() > 200 {
            self.messages.drain(..self.messages.len() - 200);
        }
    }
}

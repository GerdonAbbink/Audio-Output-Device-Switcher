//! Background thread: polls Sonar (or Windows) and executes switch commands,
//! so HTTP/COM calls never block the tray's message loop.

use std::collections::HashMap;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use crate::model::{Backend, Device, Kind, Snapshot, kind_from_name};
use crate::sonar::{Mode, Sonar};
use crate::winaudio::WinAudio;

const POLL: Duration = Duration::from_millis(1500);
const REDISCOVER: Duration = Duration::from_secs(5);

pub enum Command {
    /// Route all output channels to this device.
    SetAll(String),
    /// Route one channel to this device.
    SetChannel(String, String),
    Refresh,
}

struct Worker {
    sonar: Option<Sonar>,
    mode: Mode,
    last_discover: Option<Instant>,
    win: Option<WinAudio>,
    kinds: HashMap<String, Kind>,
}

impl Worker {
    fn kind(&mut self, id: &str, name: &str) -> Kind {
        if let Some(k) = self.kinds.get(id) {
            return *k;
        }
        // Name hints win over form factor: many USB headsets report "Speakers".
        let by_name = kind_from_name(name);
        let k = if by_name == Kind::Headset {
            Kind::Headset
        } else {
            self.win.as_ref().and_then(|w| w.form_factor_kind(id)).unwrap_or(by_name)
        };
        self.kinds.insert(id.to_string(), k);
        k
    }

    fn devices(&mut self, list: Vec<(String, String)>) -> Vec<Device> {
        list.into_iter()
            .map(|(id, name)| {
                let kind = self.kind(&id, &name);
                Device { id, name, kind }
            })
            .collect()
    }

    fn ensure_sonar(&mut self, force: bool) {
        if self.sonar.is_some() {
            return;
        }
        let due = force || self.last_discover.map_or(true, |t| t.elapsed() >= REDISCOVER);
        if !due {
            return;
        }
        self.last_discover = Some(Instant::now());
        match Sonar::discover() {
            Ok(s) => self.sonar = Some(s),
            Err(e) => log(&format!("sonar discovery: {}", e.0)),
        }
    }

    fn poll_sonar(&mut self) -> Option<Snapshot> {
        let s = self.sonar.as_ref()?;
        let res = (|| {
            let mode = s.mode()?;
            let names = if mode == Mode::Streamer { s.all_output_names()? } else { vec![] };
            Ok::<_, crate::sonar::SonarError>((mode, s.output_devices()?, s.routing(mode)?, names))
        })();
        match res {
            Ok((mode, devs, channels, other_names)) => {
                self.mode = mode;
                let devices = self.devices(devs);
                Some(Snapshot { backend: Backend::Sonar(mode), devices, channels, other_names })
            }
            Err(e) => {
                log(&format!("sonar poll failed, will rediscover: {}", e.0));
                self.sonar = None;
                None
            }
        }
    }

    fn poll_windows(&mut self) -> Snapshot {
        let Some(w) = self.win.as_ref() else { return Snapshot::offline() };
        let (Ok(list), Ok(default)) = (w.output_devices(), w.default_output()) else {
            return Snapshot::offline();
        };
        let list = list.into_iter().filter(|(_, n)| !n.starts_with("SteelSeries Sonar")).collect();
        let devices = self.devices(list);
        Snapshot { backend: Backend::Windows, devices, channels: vec![("default".into(), default)], other_names: vec![] }
    }

    fn poll(&mut self, force: bool) -> Snapshot {
        self.ensure_sonar(force);
        if let Some(snap) = self.poll_sonar() {
            return snap;
        }
        self.poll_windows()
    }

    fn apply(&mut self, cmd: Command) {
        let result = match (&self.sonar, &cmd) {
            (_, Command::Refresh) => Ok(()),
            (Some(s), Command::SetAll(id)) => s.set_all(self.mode, id).map_err(|e| e.0),
            (Some(s), Command::SetChannel(ch, id)) => s.set_channel(self.mode, ch, id).map_err(|e| e.0),
            (None, Command::SetAll(id) | Command::SetChannel(_, id)) => match &self.win {
                Some(w) => w.set_default_output(id).map_err(|e| e.to_string()),
                None => Err("no audio backend".into()),
            },
        };
        if let Err(e) = result {
            log(&format!("switch failed: {e}"));
            self.sonar = None;
        }
    }
}

pub fn log(msg: &str) {
    #[cfg(debug_assertions)]
    eprintln!("[audio-output-device-switcher] {msg}");
    #[cfg(not(debug_assertions))]
    let _ = msg;
}

/// Start the worker; `notify` is called with every new snapshot.
pub fn spawn(notify: impl Fn(Snapshot) + Send + 'static) -> Sender<Command> {
    let (tx, rx) = mpsc::channel::<Command>();
    std::thread::spawn(move || {
        let win = WinAudio::new().map_err(|e| log(&format!("core audio: {e}"))).ok();
        let mut w = Worker { sonar: None, mode: Mode::Classic, last_discover: None, win, kinds: HashMap::new() };
        let mut last: Option<Snapshot> = None;
        let mut force = true;
        loop {
            let mut commanded = false;
            match rx.recv_timeout(if force { Duration::ZERO } else { POLL }) {
                Ok(cmd) => {
                    w.apply(cmd);
                    commanded = true;
                    force = true; // re-discover right away if the command lost Sonar
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            let snap = w.poll(force);
            force = false;
            // After a command always notify, so the menu's check marks get rebuilt.
            if commanded || last.as_ref() != Some(&snap) {
                notify(snap.clone());
                last = Some(snap);
            }
        }
    });
    tx
}

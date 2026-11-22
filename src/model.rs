use crate::sonar::Mode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Headset,
    Speakers,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub kind: Kind,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Backend {
    Sonar(Mode),
    Windows,
    Offline,
}

/// What the tray icon should show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Status {
    Headset,
    Speakers,
    Mixed,
    Unknown,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub backend: Backend,
    pub devices: Vec<Device>,
    /// `(channel, device id)`; for the Windows backend a single `("default", id)`.
    pub channels: Vec<(String, String)>,
    /// Names of devices that can't be selected but may be routed to
    /// (e.g. Sonar's own virtual stream device).
    pub other_names: Vec<(String, String)>,
}

pub fn channel_label(ch: &str) -> &str {
    match ch {
        "game" => "Game",
        "chat" => "Chat",
        "media" => "Media",
        "aux" => "Aux",
        "monitoring" => "Monitoring (what you hear)",
        "streaming" => "Streaming (stream mix)",
        "default" => "Default",
        other => other,
    }
}

/// Device name without the driver suffix: "Speakers (Realtek(R) Audio)" -> "Speakers".
/// "SteelSeries Sonar - Stream (...)" -> "Stream".
pub fn short_name(name: &str) -> &str {
    let base = name.split(" (").next().unwrap_or(name);
    base.strip_prefix("SteelSeries Sonar - ").unwrap_or(base)
}

/// Fallback when the Windows form factor can't be read.
pub fn kind_from_name(name: &str) -> Kind {
    let n = name.to_lowercase();
    let headset_words = ["headphone", "headset", "earphone", "earbud", "buds", "airpods", "arctis", "hands-free"];
    if headset_words.iter().any(|w| n.contains(w)) { Kind::Headset } else { Kind::Speakers }
}

impl Snapshot {
    pub fn offline() -> Self {
        Snapshot { backend: Backend::Offline, devices: vec![], channels: vec![], other_names: vec![] }
    }

    /// Display name for any device id, or `None` if nothing is routed.
    pub fn any_name(&self, id: &str) -> Option<String> {
        if id.is_empty() {
            return None;
        }
        self.device(id)
            .map(|d| d.name.clone())
            .or_else(|| self.other_names.iter().find(|(i, _)| i == id).map(|(_, n)| n.clone()))
            .or_else(|| Some("Unknown device".into()))
    }

    /// Channels that decide the icon and that "All outputs" switches.
    pub fn primary_channels(&self) -> impl Iterator<Item = &(String, String)> {
        let streamer = self.backend == Backend::Sonar(Mode::Streamer);
        self.channels.iter().filter(move |(ch, _)| !streamer || ch == "monitoring")
    }

    pub fn device(&self, id: &str) -> Option<&Device> {
        self.devices.iter().find(|d| d.id == id)
    }

    /// The device every primary channel goes to, if they all agree.
    pub fn all_device(&self) -> Option<&str> {
        let mut ids = self.primary_channels().map(|(_, id)| id.as_str());
        let first = ids.next()?;
        if first.is_empty() || !ids.all(|id| id == first) {
            return None;
        }
        Some(first)
    }

    pub fn status(&self) -> Status {
        if let Some(id) = self.all_device() {
            return match self.device(id).map(|d| d.kind) {
                Some(Kind::Headset) => Status::Headset,
                Some(Kind::Speakers) => Status::Speakers,
                None => Status::Unknown,
            };
        }
        if self.primary_channels().any(|(_, id)| !id.is_empty()) { Status::Mixed } else { Status::Unknown }
    }

    fn name_of(&self, id: &str) -> String {
        self.any_name(id).unwrap_or_else(|| "none".into())
    }

    pub fn tooltip(&self) -> String {
        let source = match self.backend {
            Backend::Sonar(Mode::Classic) => "Sonar (classic)",
            Backend::Sonar(Mode::Streamer) => "Sonar (streamer)",
            Backend::Windows => "Windows default device",
            Backend::Offline => return "Audio output: not available".into(),
        };
        let body = match self.all_device() {
            Some(id) => self.name_of(id),
            None => self
                .primary_channels()
                .map(|(ch, id)| format!("{}: {}", channel_label(ch), short_name(&self.name_of(id))))
                .collect::<Vec<_>>()
                .join(", "),
        };
        let mut t = format!("{body}\n{source}");
        // Shell tooltips are limited to 127 characters.
        if t.chars().count() > 127 {
            t = t.chars().take(124).collect::<String>() + "...";
        }
        t
    }
}

//! Client for the (unofficial, local) SteelSeries Sonar web API.
//!
//! Discovery: `coreProps.json` -> `ggEncryptedAddress` (HTTPS, self-signed)
//! -> `GET /subApps` -> `subApps.sonar.metadata.webServerAddress` (plain HTTP).
//! The Sonar port changes every time GG restarts, so callers re-discover on error.

use serde::Deserialize;
use std::time::Duration;

/// Output channels that make up "all output devices" in classic mode.
pub const CLASSIC_OUTPUTS: [&str; 4] = ["game", "chat", "media", "aux"];
/// Streamer mode: "monitoring" is what you hear, "streaming" is the stream mix.
pub const STREAM_OUTPUTS: [&str; 2] = ["monitoring", "streaming"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Classic,
    Streamer,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioDevice {
    pub friendly_name: Option<String>,
    pub id: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub is_vad: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClassicRedirection {
    id: String,
    device_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StreamRedirection {
    stream_redirection_id: String,
    device_id: Option<String>,
}

pub struct Sonar {
    base: String,
    agent: ureq::Agent,
}

#[derive(Debug)]
pub struct SonarError(pub String);

impl<E: std::fmt::Display> From<E> for SonarError {
    fn from(e: E) -> Self {
        SonarError(e.to_string())
    }
}

type R<T> = Result<T, SonarError>;

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(3))
        .build()
}

/// Encode the characters in a Windows endpoint id that are not path-safe.
fn enc(s: &str) -> String {
    s.replace('{', "%7B").replace('}', "%7D").replace('#', "%23")
}

impl Sonar {
    /// Locate the Sonar web server via SteelSeries GG.
    pub fn discover() -> R<Self> {
        let program_data = std::env::var("ProgramData").unwrap_or_else(|_| r"C:\ProgramData".into());
        let candidates = [
            format!(r"{program_data}\SteelSeries\GG\coreProps.json"),
            format!(r"{program_data}\SteelSeries\SteelSeries Engine 3\coreProps.json"),
        ];
        let props = candidates
            .iter()
            .find_map(|p| std::fs::read_to_string(p).ok())
            .ok_or_else(|| SonarError("coreProps.json not found (is SteelSeries GG installed?)".into()))?;
        let props: serde_json::Value = serde_json::from_str(&props)?;
        let gg = props["ggEncryptedAddress"]
            .as_str()
            .ok_or_else(|| SonarError("ggEncryptedAddress missing".into()))?;

        // GG uses a self-signed certificate on localhost.
        let tls = native_tls::TlsConnector::builder()
            .danger_accept_invalid_certs(true)
            .danger_accept_invalid_hostnames(true)
            .build()?;
        let gg_agent = ureq::AgentBuilder::new()
            .tls_connector(std::sync::Arc::new(tls))
            .timeout(Duration::from_secs(3))
            .build();
        let sub: serde_json::Value = gg_agent.get(&format!("https://{gg}/subApps")).call()?.into_json()?;
        let sonar = &sub["subApps"]["sonar"];
        if !(sonar["isEnabled"].as_bool().unwrap_or(false) && sonar["isRunning"].as_bool().unwrap_or(false)) {
            return Err(SonarError("Sonar is not enabled/running".into()));
        }
        if !sonar["isReady"].as_bool().unwrap_or(false) {
            return Err(SonarError("Sonar is not ready yet".into()));
        }
        let web = sonar["metadata"]["webServerAddress"].as_str().unwrap_or("");
        if web.is_empty() || web == "null" {
            return Err(SonarError("Sonar webServerAddress not published".into()));
        }
        Ok(Sonar { base: web.trim_end_matches('/').to_string(), agent: agent() })
    }

    fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> R<T> {
        Ok(self.agent.get(&format!("{}/{}", self.base, path)).call()?.into_json()?)
    }

    fn put(&self, path: &str) -> R<()> {
        self.agent
            .put(&format!("{}/{}", self.base, path))
            .set("Content-Length", "0")
            .call()?;
        Ok(())
    }

    pub fn mode(&self) -> R<Mode> {
        let m: String = self.get("mode")?;
        Ok(if m.eq_ignore_ascii_case("stream") { Mode::Streamer } else { Mode::Classic })
    }

    /// Physical (non-Sonar-virtual) active playback devices.
    pub fn output_devices(&self) -> R<Vec<(String, String)>> {
        let list: Vec<AudioDevice> = self.get("AudioDevices?deviceDataFlow=0&removeSteelSeriesVAD=true")?;
        Ok(list
            .into_iter()
            .filter(|d| !d.is_vad && d.state.as_deref().map_or(true, |s| s == "active"))
            .filter_map(|d| Some((d.id?, d.friendly_name.unwrap_or_else(|| "Unknown device".into()))))
            .collect())
    }

    /// Names of every playback device, including Sonar's virtual ones
    /// (the stream mix usually goes to one of those).
    pub fn all_output_names(&self) -> R<Vec<(String, String)>> {
        let list: Vec<AudioDevice> = self.get("AudioDevices?deviceDataFlow=0")?;
        Ok(list
            .into_iter()
            .filter_map(|d| Some((d.id?, d.friendly_name.unwrap_or_default())))
            .collect())
    }

    /// `(channel, device id)` for every output channel of the given mode.
    pub fn routing(&self, mode: Mode) -> R<Vec<(String, String)>> {
        Ok(match mode {
            Mode::Classic => {
                let r: Vec<ClassicRedirection> = self.get("ClassicRedirections")?;
                CLASSIC_OUTPUTS
                    .iter()
                    .map(|ch| {
                        let dev = r.iter().find(|x| x.id == *ch).and_then(|x| x.device_id.clone());
                        (ch.to_string(), dev.unwrap_or_default())
                    })
                    .collect()
            }
            Mode::Streamer => {
                let r: Vec<StreamRedirection> = self.get("StreamRedirections")?;
                STREAM_OUTPUTS
                    .iter()
                    .map(|ch| {
                        let dev = r
                            .iter()
                            .find(|x| x.stream_redirection_id == *ch)
                            .and_then(|x| x.device_id.clone());
                        (ch.to_string(), dev.unwrap_or_default())
                    })
                    .collect()
            }
        })
    }

    pub fn set_channel(&self, mode: Mode, channel: &str, device_id: &str) -> R<()> {
        let root = match mode {
            Mode::Classic => "ClassicRedirections",
            Mode::Streamer => "StreamRedirections",
        };
        self.put(&format!("{root}/{channel}/deviceId/{}", enc(device_id)))
    }

    /// "All output devices": route every listening channel to one device.
    /// In streamer mode that is the monitoring mix (what you hear).
    pub fn set_all(&self, mode: Mode, device_id: &str) -> R<()> {
        match mode {
            Mode::Classic => {
                for ch in CLASSIC_OUTPUTS {
                    self.set_channel(mode, ch, device_id)?;
                }
                Ok(())
            }
            Mode::Streamer => self.set_channel(mode, "monitoring", device_id),
        }
    }
}

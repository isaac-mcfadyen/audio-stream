use regex::Regex;
use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiscoveryEvent {
	Discovered,
	Lost,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DiscoveryResult {
	pub event: DiscoveryEvent,
	pub name: String,
	pub addr: String,
}

pub struct StreamDiscoverer {
	rx: mpsc::Receiver<eyre::Result<DiscoveryResult>>,
}
impl StreamDiscoverer {
	pub fn new() -> eyre::Result<Self> {
		let (tx, mut rx) = mpsc::channel::<eyre::Result<DiscoveryResult>>(10);
		tokio::task::spawn(async move {
			let result: eyre::Result<()> = async {
				let mut process = tokio::process::Command::new("audio-stream")
					.args(["discover", "-p", "9080", "--json"])
					.stdout(std::process::Stdio::piped())
					.spawn()?;

				let stdout = process.stdout.take().unwrap();
				let mut reader = BufReader::new(stdout);
				loop {
					let mut line = String::new();
					let n = reader.read_line(&mut line).await?;
					if n == 0 {
						break;
					}
					let line = line.trim();
					let result = serde_json::from_str::<DiscoveryResult>(&line)?;
					tx.send(Ok(result)).await?;
				}

				Err(eyre::eyre!("Discovery task died"))
			}.await;
			if let Err(err) = result {
				tx.send(Err(eyre::eyre!("Error in discovery task: {}", err)))
					.await
					.ok();
			}
		});
		Ok(Self { rx })
	}
	pub async fn recv(&mut self) -> Option<eyre::Result<DiscoveryResult>> {
		self.rx.recv().await
	}
}

#[derive(Debug, Clone)]
pub struct StreamStats {
	pub sample_rate: String,
	pub buffer_size: String,
	pub bitrate: String,
}

pub struct ConnectedStream {
	process: Child,
}
impl ConnectedStream {
	pub fn connect(addr: String) -> eyre::Result<(Self, ConnectedStreamStatsReceiver)> {
		let mut task = Command::new("audio-stream")
			.args([
				"send",
				"-p", "9080",
				"-d", "BlackHole 2ch",
				"-a", &addr,
				"--sample-rate", "44100",
				"--buffer-size", "2048"
			])
			.stdout(std::process::Stdio::piped())
			.spawn()?;

		// Spawn a task to receive stats from stdout.
		let (tx, rx) = mpsc::channel(10);
		let stdout = task.stdout.take().unwrap();
		tokio::task::spawn(async move {
			let mut reader = BufReader::new(stdout);
			let sr_regex = Regex::new(r"(\d+) Hz").unwrap();
			let buffer_size_regex = Regex::new(r"(\d+) samples/buffer").unwrap();
			let bitrate_regex = Regex::new(r"(\d+) kbps").unwrap();
			loop {
				let mut line = Vec::new();
				let Ok(n) = reader.read_until(b'\r', &mut line).await else {
					break;
				};
				if n == 0 {
					break;
				}
				let line = String::from_utf8_lossy(&line);
				let line = line.trim();

				// Parse stats.
				let mut sr = None;
				let mut buffer_size = None;
				let mut bitrate = None;
				if let Some(captures) = sr_regex.captures(line) {
					sr = Some(captures.get(1).unwrap().as_str().to_string());
				}
				if let Some(captures) = buffer_size_regex.captures(line) {
					buffer_size = Some(captures.get(1).unwrap().as_str().to_string());
				}
				if let Some(captures) = bitrate_regex.captures(line) {
					bitrate = Some(captures.get(1).unwrap().as_str().to_string());
				}

				if let (Some(sr), Some(buffer_size), Some(bitrate)) = (sr, buffer_size, bitrate) {
					tx.send(Ok(StreamStats { sample_rate: sr, buffer_size, bitrate })).await.ok();
				}
			}
		});

		Ok((Self { process: task }, ConnectedStreamStatsReceiver { rx }))
	}
	pub fn kill(mut self) {
		tokio::task::spawn(async move {
			// Kill the process.
			self.process.kill().await.ok();
		});
	}
}
pub struct ConnectedStreamStatsReceiver {
	rx: mpsc::Receiver<eyre::Result<StreamStats>>,
}
impl ConnectedStreamStatsReceiver {
	pub async fn recv(&mut self) -> Option<eyre::Result<StreamStats>> {
		self.rx.recv().await
	}
}
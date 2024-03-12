use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use moka::future::Cache;
use serde_json::json;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::sync::mpsc::Receiver;

#[derive(Debug, Clone)]
pub struct DiscoveryResult {
	pub name: String,
	pub addr: SocketAddr,
}

#[derive(Debug, Clone)]
struct DiscoveryMessage {
	pub name: String,
}

impl DiscoveryMessage {
	pub fn from_buffer(buffer: &[u8]) -> eyre::Result<Self> {
		let name_len = buffer[0] as usize;
		let name = String::from_utf8(buffer[1..name_len + 1].to_vec())?;
		Ok(Self {
			name,
		})
	}
	pub fn to_buffer(&self) -> eyre::Result<Vec<u8>> {
		let name = self.name.clone().into_bytes();
		let mut buffer = vec![];
		buffer.push(name.len() as u8);
		buffer.extend(name);
		Ok(buffer)
	}
}

pub struct Discovery;

impl Discovery {
	pub async fn start(addr: SocketAddr, name: String) -> eyre::Result<()> {
		let listener = UdpSocket::bind(addr).await?;

		let expected_message = "audio-stream discovery";
		let mut buf = [0; 1024];
		loop {
			let (len, other_addr) = listener.recv_from(&mut buf).await?;
			if len != expected_message.len() {
				continue;
			}
			if &buf[..len] != expected_message.as_bytes() {
				continue;
			}

			// Send back the discovery message.
			let message = DiscoveryMessage {
				name: name.clone(),
			};
			listener.send_to(&message.to_buffer()?, other_addr).await?;
		}
	}
	async fn send_discovery(socket: Arc<UdpSocket>, port: u16) -> eyre::Result<()> {
		socket.set_broadcast(true)?;
		socket.send_to(
			"audio-stream discovery".as_bytes(),
			format!("255.255.255.255:{}", port),
		).await?;
		Ok(())
	}
	async fn recv_discovery(socket: Arc<UdpSocket>) -> eyre::Result<Receiver<eyre::Result<DiscoveryResult>>> {
		let mut buf = [0; 1024];
		let (tx, rx) = mpsc::channel(1);
		tokio::task::spawn(async move {
			let result: eyre::Result<()> = async {
				loop {
					let (len, other_addr) = socket.recv_from(&mut buf).await?;
					let message = DiscoveryMessage::from_buffer(&buf[..len])?;
					tx.send(Ok(DiscoveryResult {
						name: message.name,
						addr: other_addr,
					})).await.ok();
				}
			}.await;
			if let Err(err) = result {
				tx.send(Err(err)).await.ok();
			}
		});
		Ok(rx)
	}

	pub async fn search(name: String, port: u16, timeout: u32) -> eyre::Result<Option<DiscoveryResult>> {
		let socket = Arc::new(UdpSocket::bind("0.0.0.0:0").await?);
		let mut rx = Self::recv_discovery(socket.clone()).await?;
		Self::send_discovery(socket.clone(), port).await?;

		let found_task = async {
			loop {
				let Some(device) = rx.recv().await else {
					return Err(eyre::eyre!("Device discovery failed."));
				};
				let device = device?;
				if device.name == name {
					return Ok(device);
				}
			}
		};
		let timeout = tokio::time::sleep(Duration::from_millis(timeout as u64));
		tokio::select! {
			device = found_task => {
				let device = device?;
				Ok(Some(device))
			}
			_ = timeout => {
				Ok(None)
			}
		}
	}
	pub async fn print_all(port: u16, timeout: u32, json: bool, ) -> eyre::Result<()> {
		let socket = Arc::new(UdpSocket::bind("0.0.0.0:0").await?);
		let mut devices = Self::recv_discovery(socket.clone()).await?;

		// Prepare a future to send discovery messages.
		let send_task = async {
			let mut interval = tokio::time::interval(Duration::from_millis(100));
			loop {
				interval.tick().await;
				Self::send_discovery(socket.clone(), port).await?;
			}
			Ok::<(), eyre::Error>(())
		};

		let found_items = Cache::builder()
			.time_to_idle(Duration::from_millis(timeout as u64))
			.eviction_listener(move |k: Arc<String>, v: DiscoveryResult, _| {
				if json {
					let json = json!({
						"event": "lost",
						"name": *k,
						"addr": v.addr,
					});
					println!("{}", json);
				} else {
					tracing::info!("  \"{}\" at {} lost", k, v.addr);
				}
			})
			.build();

		// Prepare a future to add new clients.
		let recv_task = async {
			if !json {
				tracing::info!("Discovered audio receivers:");
			}
			loop {
				let Some(device) = devices.recv().await else {
					return Err(eyre::eyre!("Device discovery failed."));
				};
				let device = device?;

				// Ignore if this device is already known.
				if found_items.get(&device.name).await.is_some() {
					continue;
				}
				found_items.insert(device.name.clone(), device.clone()).await;

				if json {
					let json = json!({
						"event": "discovered",
						"name": device.name,
						"addr": device.addr,
					});
					println!("{}", json);
				} else {
					tracing::info!("  \"{}\" at {} discovered", device.name, device.addr);
				}
			}
			Ok::<(), eyre::Error>(())
		};
		tokio::select! {
			_ = send_task => {
				Ok(())
			}
			_ = recv_task => {
				Ok(())
			}
		}
	}
}
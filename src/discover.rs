use std::net::{IpAddr, SocketAddr};
use tokio::net::UdpSocket;

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
	pub async fn discover(addr: IpAddr, port: u16, timeout: Option<u32>) -> eyre::Result<()> {
		let socket = UdpSocket::bind(
			format!("{}:0", addr)
		).await?;

		// Send the discovery message.
		socket.set_broadcast(true)?;
		socket.send_to(
			"audio-stream discovery".as_bytes(),
			format!(
				"{}:{}",
				if addr.is_ipv4() {
					"255.255.255.255"
				} else {
					"[ff02::1]"
				},
				port
			),
		).await?;

		// Wait for 1s for replies.
		tracing::info!("Discovered audio receivers:");
		let recv_task = async {
			let mut buf = [0; 1024];
			loop {
				let (len, other_addr) = socket.recv_from(&mut buf).await?;
				let message = DiscoveryMessage::from_buffer(&buf[..len])?;
				tracing::info!("  \"{}\" at {}", message.name, other_addr);
			}
			Ok::<(), eyre::Error>(())
		};
		let timeout = tokio::time::sleep(std::time::Duration::from_millis(
			// Max delay is 2.2 years, should be long enough.
			timeout.map(|v| v as u64).unwrap_or(68719476734)
		));

		tokio::select! {
			_ = recv_task => {
				Ok(())
			}
			_ = timeout => {
				Ok(())
			}
		}
	}
}
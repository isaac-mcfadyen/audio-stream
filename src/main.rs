use std::io::Write;
use std::net::SocketAddr;
use std::str::FromStr;
use clap::{Args, Parser, Subcommand};
use tokio::io::{BufWriter, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, AsyncReadExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::Instant;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::filter::Directive;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use crate::discover::Discovery;
use crate::input::AudioInput;
use crate::output::AudioOutput;

mod input;
mod output;
mod discover;

const CLEAR_LINE: &str = "\x1B[K";
const FLUSH_LINE: &str = "\r";
const BLUE: &str = "\x1B[34m";
const CLEAR_COLOR: &str = "\x1B[0m";

#[derive(Parser, Debug)]
struct ProgramArgs {
	#[clap(subcommand)]
	command: Command,

	#[clap(global = true, short = 'q', long)]
	/// Quiet mode. Suppresses output of stats.
	quiet: bool,
}

#[derive(Subcommand, Debug)]
enum Command {
	Discover {
		#[arg(short = 'p', long)]
		/// The port to search for audio receivers on.
		port: u16,
		#[arg(short = 't', long, default_value_t = 1000)]
		/// The time in milliseconds after which devices are considered lost.
		timeout: u32,
		#[arg(long, default_value_t = false)]
		/// Output in machine-readable JSON.
		json: bool,
	},
	Send {
		#[arg(short = 'p', long)]
		/// The port to use for discovery or connection.
		port: u16,
		#[command(flatten)]
		connect: ConnectArgs,

		#[arg(short = 'd', long)]
		/// The audio device to use.
		device: String,
		#[arg(short = 'r', long, default_value_t = 44100)]
		/// The sample rate to use. Must be supported by the audio devices of both the sender and receiver.
		sample_rate: u32,
		#[arg(short = 'b', long, default_value_t = 2048)]
		/// The buffer size to use. Must be supported by the audio devices of both the sender and receiver.
		/// A longer buffer will increase latency but reduce the chance of audio artifacts.
		buffer_size: u32,
	},
	Recv {
		#[arg(long)]
		/// Whether to enable network discovery
		enable_discovery: bool,
		#[arg(long, required_if_eq("enable_discovery", "true"))]
		/// The name of this audio receiver, used to identify it in discovery messages.
		discovery_name: Option<String>,

		#[arg(short = 'd', long)]
		/// The audio device to use.
		device: String,
		#[arg(short = 'l', long)]
		/// The address to listen on.
		listen_address: SocketAddr,
	},
}

#[derive(Debug, Args)]
#[group(required = true, multiple = false)]
struct ConnectArgs {
	#[arg(short = 'a', long)]
	/// The address to connect to.
	addr: Option<SocketAddr>,
	#[arg(short = 'n', long)]
	/// The name of a receiver to connect to.
	name: Option<String>,
}

#[derive(Debug, Clone)]
struct Handshake {
	sample_rate: u32,
	buffer_size: u32,
}

impl Handshake {
	/// Writes the handshake to the given writer. Returns the number of bytes written.
	pub async fn write<T>(&self, buffer: &mut T) -> eyre::Result<u32>
		where T: AsyncWrite + Unpin
	{
		buffer.write_u32(self.sample_rate).await?;
		buffer.write_u32(self.buffer_size).await?;
		buffer.flush().await?;
		Ok(8)
	}
	pub async fn read<T>(buffer: &mut T) -> eyre::Result<Self>
		where T: AsyncRead + Unpin
	{
		let sample_rate = buffer.read_u32().await?;
		let buffer_size = buffer.read_u32().await?;
		Ok(Self { sample_rate, buffer_size })
	}
}

#[derive(Debug)]
struct AudioMessage<'a> {
	data: &'a [f32],
	timestamp: u64,
	fully_silent: bool,
}

impl<'a> AudioMessage<'a> {
	/// Writes the audio message to the given writer. Returns the number of bytes written.
	pub async fn write<T>(
		&self,
		buffer: &mut T,
	) -> eyre::Result<u32>
		where T: AsyncWrite + Unpin
	{
		buffer.write_u64(self.timestamp).await?;
		buffer.write_u8(if self.fully_silent { 1 } else { 0 }).await?;
		buffer.write_u64(self.data.len() as u64).await?;
		for sample in self.data {
			buffer.write_f32_le(*sample).await?;
		}
		buffer.flush().await?;
		Ok((self.data.len() * 4 + 8) as u32)
	}
	/// Reads the audio buffer from the given reader. Returns the number of bytes read and the audio message.
	pub async fn read<T>(buffer: &'a mut [f32], reader: &mut T) -> eyre::Result<(u32, Self)>
		where T: AsyncRead + Unpin
	{
		let mut num_read = 0;
		let timestamp = reader.read_u64().await?;
		num_read += 8;

		let fully_silent = reader.read_u8().await? == 1;
		num_read += 1;

		let length = reader.read_u64().await? as usize;
		num_read += 8;

		if length > buffer.len() {
			return Err(eyre::eyre!("Buffer too small to read message."));
		}
		for sample in buffer.iter_mut().take(length) {
			*sample = reader.read_f32_le().await?;
			num_read += 4;
		}
		Ok((num_read, Self {
			data: buffer,
			fully_silent,
			timestamp,
		}))
	}
}

#[tokio::main]
async fn main() -> eyre::Result<()> {
	tracing_subscriber::registry()
		.with(tracing_subscriber::fmt::layer())
		.with(
			EnvFilter::builder()
				.with_default_directive(Directive::from_str("audio_stream=debug").unwrap())
				.from_env()
				.unwrap(),
		)
		.init();

	let args = ProgramArgs::parse();
	match args.command {
		Command::Discover { port, timeout, json } => {
			Discovery::print_all(port, timeout, json).await?;
		}
		Command::Send { device, port, connect, sample_rate, buffer_size } => {
			let mut input = AudioInput::new(
				device,
				sample_rate,
				buffer_size,
			).await?;

			loop {
				let connection = if let Some(addr) = connect.addr {
					let Ok(connection) = TcpStream::connect(addr).await else {
						tracing::warn!("Failed to connect to {}, retrying in 5s.", addr);
						tokio::time::sleep(std::time::Duration::from_secs(5)).await;
						continue;
					};
					connection
				} else if let Some(name) = connect.name.clone() {
					let Some(device) = Discovery::search(name.clone(), port, 2000).await? else {
						tracing::warn!("Failed to find device with name {}, retrying in 5s.", name);
						tokio::time::sleep(std::time::Duration::from_secs(5)).await;
						continue;
					};
					tracing::info!("Found device with name \"{}\" at {}", name, device.addr);
					TcpStream::connect(device.addr).await?
				} else {
					unreachable!();
				};
				let mut writer = BufWriter::new(connection);

				// Send the initial handshake.
				let handshake = Handshake {
					sample_rate,
					buffer_size: input.buffer_size(),
				};
				handshake.write(&mut writer).await?;

				// Flush the input buffer.
				input.flush();

				let start_time = Instant::now();
				let mut num_written = 0;
				let mut last_bits_sec = 0.0;
				let mut buffer = vec![0.0; input.buffer_size() as usize];
				loop {
					// Read audio.
					input.pop_slice(&mut buffer).await?;

					// Encode and send the message.
					let fully_silent = buffer.iter().all(|&v| v == 0.0);
					let message = AudioMessage {
						data: if fully_silent { &[] } else { &buffer },
						fully_silent,
						timestamp: std::time::SystemTime::now()
							.duration_since(std::time::UNIX_EPOCH)
							.unwrap()
							.as_millis() as u64,
					};
					let Ok(n) = message.write(&mut writer).await else {
						tracing::error!("Socket disconnected.");
						break;
					};

					// Calculate bits per second.
					num_written += n;
					let bits_sec = (num_written as f64 / start_time.elapsed().as_secs_f64()) * 8.0;
					if last_bits_sec == 0.0 {
						last_bits_sec = bits_sec;
					}
					let bits_sec = 0.1 * bits_sec + (1.0 - 0.1) * last_bits_sec;
					last_bits_sec = bits_sec;

					// Put some stats on screen.
					if !args.quiet {
						print!(
							"{CLEAR_LINE}[{BLUE}CONNECTED{CLEAR_COLOR}] Sender | {} Hz, {} samples/buffer, {}{FLUSH_LINE}",
							sample_rate,
							buffer_size,
							if fully_silent {
								"1 kbps, silence detected".to_string()
							} else {
								format!("{:.0} kbps", bits_sec / 1024.0)
							}
						);
						std::io::stdout().flush().unwrap();
					}
				}
			}
		}
		Command::Recv { device, listen_address, enable_discovery, discovery_name } => {
			// Spawn a task to reply to discovery messages.
			if enable_discovery {
				let Some(discovery_name) = discovery_name.clone() else {
					tracing::error!("Discovery name must be provided when discovery is enabled.");
					return Err(eyre::eyre!("Discovery name must be provided when discovery is enabled."));
				};
				tokio::task::spawn(Discovery::start(listen_address, discovery_name));
			}

			// Spawn a TCP listener for incoming connections.
			let listener = TcpListener::bind(&listen_address).await?;
			tracing::info!("Listening for connections on {}", listen_address);

			loop {
				let (socket, addr) = listener.accept().await?;
				tracing::info!("Accepted connection from {}", addr);
				let mut reader = BufReader::new(socket);

				// Read the initial handshake.
				let handshake = Handshake::read(&mut reader).await?;

				// Create the output device based on the handshake.
				let mut output = AudioOutput::new(
					device.clone(),
					handshake.sample_rate,
					handshake.buffer_size,
				).await?;

				let empty_buffer = vec![0.0; output.buffer_size() as usize];
				let mut buffer = vec![0.0; output.buffer_size() as usize];
				let mut num_read = 0;
				let mut last_latency = 0.0;
				let mut last_bits_sec = 0.0;
				let start_time = Instant::now();

				// Push a buffer of silence to start. This step helps ensure there's a buffer,
				// because if there's no buffer then there's a higher risk of audio artifacts.
				output.push_slice(&empty_buffer).await?;
				loop {
					// Read the message.
					let (n, message) = match AudioMessage::read(&mut buffer, &mut reader).await {
						Ok(v) => v,
						Err(err) => {
							// Write an empty audio buffer to avoid audio artifacts.
							output.push_slice(&vec![0.0; output.buffer_size() as usize]).await?;

							tracing::error!("Socket disconnected: {}", err);
							break;
						}
					};

					if message.fully_silent {
						// Write an empty audio buffer to avoid audio artifacts.
						output.push_slice(&empty_buffer).await?;
					} else {
						// Write the audio to the output device.
						output.push_slice(message.data).await?;
					}

					// Calculate latency, smoothed using EMA.
					let latency = (std::time::SystemTime::now()
						.duration_since(std::time::UNIX_EPOCH)
						.unwrap()
						.as_millis() as u64 - message.timestamp) as f32;
					if last_latency == 0.0 {
						last_latency = latency;
					}
					let latency = 0.1 * latency + (1.0 - 0.1) * last_latency;
					last_latency = latency;

					// Calculate bits per second.
					num_read += n;
					let bits_sec = (num_read as f64 / start_time.elapsed().as_secs_f64()) * 8.0;
					if last_bits_sec == 0.0 {
						last_bits_sec = bits_sec;
					}
					let bits_sec = 0.1 * bits_sec + (1.0 - 0.1) * last_bits_sec;
					last_bits_sec = bits_sec;

					// Put some stats on the screen.
					if !args.quiet {
						print!(
							"{CLEAR_LINE}[{BLUE}CONNECTED{CLEAR_COLOR}] Receiving from {}, {:.0}ms network latency | {} Hz, {} samples/buffer, {}{FLUSH_LINE}",
							addr,
							latency,
							handshake.sample_rate,
							handshake.buffer_size,
							if message.fully_silent {
								"1 kbps, silence detected".to_string()
							} else {
								format!("{:.0} kbps", bits_sec / 1024.0)
							}
						);
						std::io::stdout().flush().unwrap();
					}
				}
			}
		}
	}
	Ok(())
}

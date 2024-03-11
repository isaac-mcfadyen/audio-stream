use std::io::Write;
use std::str::FromStr;
use clap::Parser;
use tokio::io::{BufWriter, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, AsyncReadExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::Instant;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::filter::Directive;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use crate::input::AudioInput;
use crate::output::AudioOutput;

mod input;
mod output;

const CLEAR_LINE: &str = "\x1B[K";
const FLUSH_LINE: &str = "\r";
const BLUE: &str = "\x1B[34m";
const CLEAR_COLOR: &str = "\x1B[0m";

#[derive(Parser, Debug)]
enum Command {
	Send {
		#[arg(short = 'd', long)]
		/// The audio device to use.
		device: String,
		#[arg(short = 'c', long)]
		/// The address to connect to.
		connect_address: String,
		#[arg(short = 'r', long, default_value_t = 44100)]
		/// The sample rate to use. Must be supported by the audio devices of both the sender and receiver.
		sample_rate: u32,
		#[arg(short = 'b', long, default_value_t = 2048)]
		/// The buffer size to use. Must be supported by the audio devices of both the sender and receiver.
		/// A longer buffer will increase latency but reduce the chance of audio artifacts.
		buffer_size: u32,
	},
	Recv {
		#[arg(short = 'd', long)]
		/// The audio device to use.
		device: String,
		#[arg(short = 'l', long)]
		/// The address to listen on.
		listen_address: String,
	},
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
		for sample in buffer.iter_mut() {
			*sample = reader.read_f32_le().await?;
			num_read += 4;
		}
		Ok((num_read, Self { data: buffer, timestamp }))
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

	let args = Command::parse();
	match args {
		Command::Send { device, connect_address, sample_rate, buffer_size } => {
			let mut input = AudioInput::new(
				device,
				sample_rate,
				buffer_size,
			).await?;

			loop {
				let Ok(connection) = TcpStream::connect(connect_address.clone()).await else {
					tracing::warn!("Failed to connect to {}, retrying in 5s.", connect_address);
					tokio::time::sleep(std::time::Duration::from_secs(5)).await;
					continue;
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
					let message = AudioMessage {
						data: &buffer,
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
					print!(
						"{CLEAR_LINE}[{BLUE}CONNECTED{CLEAR_COLOR}] SEND to {} | {} Hz, {} samples/buffer, {:.0} kbps{FLUSH_LINE}",
						connect_address,
						sample_rate,
						buffer_size,
						bits_sec / 1024.0
					);
					std::io::stdout().flush().unwrap();
				}
			}
		}
		Command::Recv { device, listen_address } => {
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

				// Push a buffer of silence to start. This step helps ensure there's a buffer,
				// because if there's no buffer then there's a higher risk of audio artifacts.
				output.push_slice(&vec![0.0; output.buffer_size() as usize]).await?;

				let mut buffer = vec![0.0; output.buffer_size() as usize];
				let mut num_read = 0;
				let mut last_latency = 0.0;
				let mut last_bits_sec = 0.0;
				let start_time = Instant::now();
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

					// Write the audio to the output device.
					output.push_slice(message.data).await?;

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
					print!(
						"{CLEAR_LINE}[{BLUE}CONNECTED{CLEAR_COLOR}] RECV from {}, {:.0}ms network latency | {} Hz, {} samples/buffer, {:.0} kbps{FLUSH_LINE}",
						addr,
						latency,
						handshake.sample_rate,
						handshake.buffer_size,
						bits_sec / 1024.0,
					);
					std::io::stdout().flush().unwrap();
				}
			}
		}
	}
}

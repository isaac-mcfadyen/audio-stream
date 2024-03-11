use std::sync::Arc;
use async_ringbuf::{AsyncHeapRb, AsyncRb};
use async_ringbuf::consumer::AsyncConsumer;
use async_ringbuf::wrap::AsyncCons;
use cpal::{BufferSize, SampleFormat, SampleRate};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use eyre::bail;
use ringbuf::consumer::Consumer;
use ringbuf::producer::Producer;
use ringbuf::storage::Heap;
use ringbuf::traits::Split;

type RingBufConsumer = AsyncCons<Arc<AsyncRb<Heap<f32>>>>;

pub struct AudioInput {
	rx: RingBufConsumer,
	buffer_size: u32,
	sample_rate: u32,
}

impl AudioInput {
	pub async fn new(
		device: String,
		sr: u32,
		buffer_size: u32,
	) -> eyre::Result<Self> {
		let host = cpal::default_host();

		// List devices.
		let devices = host.input_devices()?;
		tracing::info!("Available input devices: ");
		for device in devices {
			tracing::info!("  {}", device.name().unwrap_or_default());
		}

		// Search for device.
		let devices = host.input_devices()?;
		let Some(device) = devices.into_iter().find(|v| v.name().unwrap_or_default() == device) else {
			tracing::error!("Device {} not found", device);
			return Err(eyre::eyre!("Device {} not found", device));
		};
		tracing::info!("Using input device: {}", device.name().unwrap_or_default());

		// Construct config.
		let config = device.default_input_config()?;
		let supported_buffer_sizes = config.buffer_size().clone();
		let sample_format = config.sample_format();
		let mut config: cpal::StreamConfig = config.into();
		config.buffer_size = BufferSize::Fixed(buffer_size);
		config.sample_rate = SampleRate(sr);
		tracing::info!("Input audio parameters chosen:");
		tracing::info!("  Sample format: {:?}", sample_format);
		tracing::info!("  Sample rate: {}", sr);
		tracing::info!("  Supported buffer sizes: {:?}", supported_buffer_sizes);
		tracing::info!("  Chosen buffer size: {}", buffer_size);

		let (spawn_tx, mut spawn_rx) = tokio::sync::mpsc::channel::<eyre::Result<()>>(1);
		let (mut tx, rx) = AsyncHeapRb::new(sr as usize * 2).split();
		std::thread::spawn(move || {
			let result = || -> eyre::Result<()>  {
				let stream = device.build_input_stream_raw(
					&config,
					sample_format,
					move |data: &cpal::Data, info: &cpal::InputCallbackInfo| {
						match data.sample_format() {
							SampleFormat::F32 => {
								let data = data.as_slice::<f32>().unwrap();
								tx.push_slice(data);
							}
							SampleFormat::I16 => {
								let data = data.as_slice::<i16>().unwrap();
								tx.push_iter(
									data.into_iter().map(|v| *v as f32 / i16::MAX as f32)
								);
							}
							_ => {
								tracing::warn!("Unsupported sample format: {:?}", data.sample_format());
							}
						}
					},
					|err| {
						tracing::error!("Error in input stream: {}", err);
					},
					None,
				)?;
				stream.play()?;

				spawn_tx.blocking_send(Ok(())).ok();
				loop {
					std::thread::park();
				}

				// Required to ensure stream isn't dropped before the end of program.
				drop(stream);
			};
			if let Err(err) = result() {
				spawn_tx.blocking_send(Err(err)).ok();
			}
		});

		spawn_rx.recv().await.ok_or(eyre::eyre!("Input thread died before sending spawn result."))??;
		Ok(Self {
			rx,
			buffer_size,
			sample_rate: sr,
		})
	}
	pub fn sample_rate(&self) -> u32 {
		self.sample_rate
	}
	pub fn buffer_size(&self) -> u32 {
		self.buffer_size
	}
	pub async fn pop_slice(&mut self, buffer: &mut [f32]) -> eyre::Result<()> {
		if self.rx.pop_exact(buffer).await.is_err() {
			bail!("Input ring buffer producer dropped.");
		}
		Ok(())
	}
	
	/// Flushes the input audio buffer.
	pub fn flush(&mut self) {
		while self.rx.try_pop().is_some() {}
	}
}
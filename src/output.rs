use std::sync::Arc;
use async_ringbuf::{AsyncHeapRb, AsyncRb};
use async_ringbuf::producer::AsyncProducer;
use async_ringbuf::wrap::AsyncProd;
use cpal::{BufferSize, SampleFormat, SampleRate};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use eyre::bail;
use ringbuf::consumer::Consumer;
use ringbuf::storage::Heap;
use ringbuf::traits::Split;

type RingBufProducer = AsyncProd<Arc<AsyncRb<Heap<f32>>>>;

pub struct AudioOutput {
	tx: RingBufProducer,
	buffer_size: u32,
}

impl AudioOutput {
	pub async fn new(
		device: String,
		sr: u32,
		buffer_size: u32,
	) -> eyre::Result<Self> {
		let host = cpal::default_host();

		// List devices.
		let devices = host.output_devices()?;
		tracing::info!("Available output devices: ");
		for device in devices {
			tracing::info!("  {}", device.name().unwrap_or_default());
		}

		// Search for device.
		let devices = host.input_devices()?;
		let Some(device) = devices.into_iter().find(|v| v.name().unwrap_or_default() == device) else {
			tracing::error!("Device {} not found", device);
			return Err(eyre::eyre!("Device {} not found", device));
		};
		tracing::info!("Using output device: {}", device.name().unwrap_or_default());

		// Construct config.
		let config = device.default_input_config()?;
		let supported_buffer_sizes = config.buffer_size().clone();
		let sample_format = config.sample_format();
		let mut config: cpal::StreamConfig = config.into();
		config.buffer_size = BufferSize::Fixed(buffer_size);
		config.sample_rate = SampleRate(sr);

		tracing::info!("Output audio parameters chosen:");
		tracing::info!("  Sample format: {:?}", sample_format);
		tracing::info!("  Sample rate: {}", sr);
		tracing::info!("  Supported buffer sizes: {:?}", supported_buffer_sizes);
		tracing::info!("  Chosen buffer size: {}", buffer_size);

		let (spawn_tx, mut spawn_rx) = tokio::sync::mpsc::channel::<eyre::Result<()>>(1);
		let (mut tx, mut rx) = AsyncHeapRb::new(sr as usize * 2).split();
		std::thread::spawn(move || {
			let result = || -> eyre::Result<()>  {
				let stream = device.build_output_stream_raw(
					&config,
					sample_format,
					move |data: &mut cpal::Data, info: &cpal::OutputCallbackInfo| {
						match data.sample_format() {
							SampleFormat::F32 => {
								let data = data.as_slice_mut::<f32>().unwrap();
								rx.pop_slice(data);
							}
							_ => {
								tracing::warn!("Unsupported output sample format: {:?}", data.sample_format());
							}
						}
					},
					|err| {
						tracing::error!("Error in output stream: {}", err);
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
			tx,
			buffer_size,
		})
	}
	pub fn buffer_size(&self) -> u32 {
		self.buffer_size
	}
	pub async fn push_slice(&mut self, buffer: &[f32]) -> eyre::Result<()> {
		if self.tx.push_exact(buffer).await.is_err() {
			bail!("Output ring buffer producer dropped.");
		}
		Ok(())
	}
}
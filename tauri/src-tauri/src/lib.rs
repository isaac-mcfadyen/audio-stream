mod stream;

use std::sync::{Arc, Mutex};
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::{AppHandle, Manager, Wry};
use crate::stream::{ConnectedStream, DiscoveryEvent, StreamDiscoverer};


#[derive(Clone)]
struct Checkbox {
	name: String,
	addr: String,
	item: CheckMenuItem<Wry>,
}

struct StatMenuItems {
	status: MenuItem<Wry>,
	sample_rate: MenuItem<Wry>,
	buffer_size: MenuItem<Wry>,
	bitrate: MenuItem<Wry>,
}
impl StatMenuItems {
	pub fn clear(&self) {
		self.status.set_text("Status: Not connected").ok();
		self.sample_rate.set_text("Sample rate: N/A").ok();
		self.buffer_size.set_text("Buffer size: N/A").ok();
		self.bitrate.set_text("Bitrate: N/A").ok();
	}
}

type CheckboxItems = Arc<Mutex<Vec<Checkbox>>>;

fn setup(app: AppHandle) -> eyre::Result<()> {
	let tray = app.tray().unwrap();

	let items = CheckboxItems::default();

	// Create a menu.
	let menu = Menu::new(&app)?;

	// Attach text for stats that we can mutate later.
	let menu_items = Arc::new(StatMenuItems {
		status: MenuItem::new(&app, "Status: Not connected", false, None::<&str>)?,
		sample_rate: MenuItem::new(&app, "Sample rate: N/A", false, None::<&str>)?,
		buffer_size: MenuItem::new(&app, "Buffer size: N/A", false, None::<&str>)?,
		bitrate: MenuItem::new(&app, "Bitrate: N/A", false, None::<&str>)?,
	});
	menu.append_items(&[
		&PredefinedMenuItem::separator(&app)?,
		&menu_items.status,
		&menu_items.sample_rate,
		&menu_items.buffer_size,
		&menu_items.bitrate
	])?;

	// Attach a handler for events.
	let stream: Arc<Mutex<Option<ConnectedStream>>> = Default::default();

	let cloned_stream = stream.clone();
	let cloned_items = items.clone();
	let cloned_menu_items = menu_items.clone();
	app.on_menu_event(move |app, event| {
		// Kill any existing stream.
		let mut stream = cloned_stream.lock().unwrap();
		if let Some(stream) = stream.take() {
			cloned_menu_items.clear();
			stream.kill();
		}

		// Find the clicked button.
		let existing_items = cloned_items.lock().unwrap();
		let Some(matching_item) = existing_items.iter().find(|v| v.item.id().0 == event.id.0) else {
			tracing::warn!("No matching item found for id {}", event.id.0);
			return;
		};
		tracing::info!("Selected: {}", matching_item.name);

		// Uncheck all other menu items.
		for item in existing_items.iter().filter(|v| v.item.id().0 != event.id.0) {
			item.item.set_checked(false).ok();
		}

		if !matching_item.item.is_checked().unwrap_or_default() {
			return;
		}

		// Start a process to send audio.
		let task = ConnectedStream::connect(matching_item.addr.clone());
		let cloned_menu_items = cloned_menu_items.clone();
		if let Ok((task, mut stats_rx)) = task {
			*stream = Some(task);

			// Set the status to connected.
			cloned_menu_items.status.set_text("Status: Active").ok();

			// Start a task to update the stats.
			tokio::task::spawn(async move {
				while let Some(stats) = stats_rx.recv().await {
					if let Err(err) = stats {
						tracing::error!("Error in stats: {}", err);
						break;
					}
					let stats = stats.unwrap();

					cloned_menu_items.sample_rate.set_text(
						format!("Sample rate: {} Hz", stats.sample_rate)
					).ok();
					cloned_menu_items.buffer_size.set_text(
						format!("Buffer size: {} samples/buffer", stats.buffer_size)
					).ok();
					cloned_menu_items.bitrate.set_text(
						format!("Bitrate: {} kbps", stats.bitrate)
					).ok();
				}
			});
		} else {
			tracing::error!("Failed to start audio stream: {}", task.err().unwrap());
		}
	});

	let mut discoverer = StreamDiscoverer::new()?;

	// Spawn a task to update the menu.
	let cloned_menu = menu.clone();
	tokio::task::spawn(async move {
		let result: eyre::Result<()> = async move {
			while let Some(result) = discoverer.recv().await {
				let result = result?;
				match result.event {
					DiscoveryEvent::Discovered => {
						let item = CheckMenuItem::new(
							&app,
							result.name.clone(),
							true,
							false,
							None::<&str>,
						)?;
						cloned_menu.prepend(&item)?;
						let mut lock = items.lock().unwrap();
						lock.push(Checkbox {
							name: result.name.clone(),
							addr: result.addr.clone(),
							item,
						});
						tracing::info!("Discovered: {}", result.name);
					}
					DiscoveryEvent::Lost => {
						let mut lock = items.lock().unwrap();
						let item = lock.iter().find(|item| item.name == result.name);
						let index = lock.iter().position(|item| item.name == result.name);
						if let (Some(index), Some(item)) = (index, item) {
							// If the item we lost was the same one that's currently playing, then
							// also kill the subprocess.
							if item.item.is_checked().unwrap_or_default() {
								let mut stream = stream.lock().unwrap();
								if let Some(stream) = stream.take() {
									stream.kill();
									menu_items.clear();
								}
							}

							cloned_menu.remove(&item.item)?;
							lock.remove(index);
						} else {
							tracing::warn!("Lost item not found: {}", result.name);
						}
						tracing::info!("Lost: {}", result.name);
					}
				}
			}
			Err(eyre::eyre!("Discovery task died"))
		}.await;
		if let Err(err) = result {
			tracing::error!("Error in discovery: {}", err);
		}
	});
	tray.set_menu(Some(menu))?;

	Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
#[tokio::main]
pub async fn run() {
	// Start tracing.
	tracing_subscriber::fmt::init();

	// Bind the tokio runtime.
	tauri::async_runtime::set(tokio::runtime::Handle::current());

	tauri::Builder::default()
		.setup(|app| {
			setup(app.app_handle().clone())?;
			Ok(())
		})
		.run(tauri::generate_context!())
		.expect("error while running tauri application");
}

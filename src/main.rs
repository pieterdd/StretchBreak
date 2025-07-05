use std::io::{BufReader, Cursor};
use std::process;
use std::sync::{Arc, Mutex};
use std::thread::{self, sleep};
use std::time::Duration as StdDuration;
mod backend;
use backend::idle_monitoring::{
    Clock, DebouncedIdleState, IdleChecker, IdleInfo, IdleMonitor, ModeState,
};
use chrono::{TimeDelta, Utc};
use clap::Parser;
use dbus::run_server;
use relm4::RelmApp;
use rodio::{Decoder, OutputStream, Sink};
use single_instance::SingleInstance;
use tokio::runtime::Runtime;
use tokio::sync::watch::{Sender, channel};
use tracing::error;
mod frontend;
use frontend::main_window::{MainWindow, MainWindowInit};
use zbus::{Connection, Error, Proxy};

use crate::backend::file_io::PersistableState;
mod dbus;

const APP_ID: &str = "io.github.pieterdd.StretchBreak";

#[derive(Debug, Clone)]
pub enum IdleMonitorMessage {
    IdleInfoUpdate(IdleInfo),
}

fn play_break_end_sound() {
    thread::spawn(|| {
        fn helper() -> Result<(), ()> {
            let (_stream, handle) = OutputStream::try_default().map_err(|_| ())?;
            let sink = Sink::try_new(&handle).map_err(|_| ())?;
            let file = BufReader::new(Cursor::new(include_bytes!("sounds/break_end.wav")));
            let decoder = Decoder::new(file).map_err(|_| ())?;
            sink.append(decoder);
            sink.sleep_until_end();
            Ok(())
        }

        match helper() {
            Ok(()) => {}
            Err(()) => {
                error!("Could not play break end sound");
            }
        }
    });
}

fn monitor_idle_forever(
    idle_monitor_ref: Arc<Mutex<IdleMonitor<IdleChecker, Clock>>>,
    idle_info_sender: Sender<IdleInfo>,
) {
    let mut previous_idle_info: Option<IdleInfo> = None;
    let mut last_state_write = Utc::now();

    loop {
        {
            let idle_info = idle_monitor_ref
                .lock()
                .expect("Idle monitor unlock failed")
                .refresh_idle_info();

            if last_state_write
                .checked_add_signed(TimeDelta::seconds(15))
                .unwrap()
                < Utc::now()
            {
                let persistable_state = idle_monitor_ref
                    .lock()
                    .expect("Idle monitor unlock failed")
                    .export_persistable_state();
                if persistable_state.save_to_disk().is_err() {
                    println!("Tried to write timer state to disk, but failed");
                }
                last_state_write = Utc::now();
            }
            idle_info_sender
                .send(idle_info)
                .expect("Could not send idle info");

            match idle_info.last_mode_state {
                ModeState::Normal { .. } => {
                    if let Some(unpacked_value) = previous_idle_info {
                        match unpacked_value.last_mode_state {
                            ModeState::Break {
                                progress_towards_finish: _,
                                idle_state,
                            } if matches!(idle_state, DebouncedIdleState::Idle { .. }) => {
                                // Silently fail if audio isn't available. Make sure we only play
                                // the sound when the user isn't skipping/postponing a break.
                                play_break_end_sound();
                            }
                            _ => {}
                        }
                    }
                }
                ModeState::PreBreak { .. } => {}
                ModeState::Break { .. } => {}
            };

            previous_idle_info = Some(idle_info);
        }

        sleep(StdDuration::from_millis(250));
    }
}

#[derive(Parser)]
#[command()]
struct Cli {
    #[arg(long)]
    hide: bool,
}

fn main() {
    #[cfg(target_os = "linux")]
    if libnotify::init(APP_ID).is_err() {
        println!("Warning: could not initialize push notifications");
    }

    let cli = Cli::parse();

    let instance = SingleInstance::new(APP_ID).expect("Initializing single instance object failed");

    if instance.is_single() {
        let persistable_state = PersistableState::load_from_disk().ok();
        if persistable_state.is_none() {
            println!("Could not read settings and timer state from disk. Loading defaults.");
        }
        let idle_monitor = IdleMonitor::new(IdleChecker, Clock, persistable_state);
        let idle_monitor_arc = Arc::new(Mutex::new(idle_monitor));
        let idle_monitor_arc2 = idle_monitor_arc.clone();
        let idle_monitor_arc3 = idle_monitor_arc.clone();

        let (idle_info_sender, idle_info_receiver) =
            channel(idle_monitor_arc.lock().unwrap().refresh_idle_info());

        let (show_main_window_sender, show_main_window_recv) = channel(!cli.hide);

        thread::spawn(move || monitor_idle_forever(idle_monitor_arc, idle_info_sender));
        let idle_info_receiver_ref = idle_info_receiver.clone();
        thread::spawn(move || {
            match run_server(
                idle_info_receiver_ref,
                show_main_window_sender,
                idle_monitor_arc3,
            ) {
                Ok(()) => {}
                Err(_) => println!("Couldn't run DBus server."),
            }
        });

        relm4_icons::initialize_icons();
        let app = RelmApp::new(APP_ID);
        app.with_args(vec![]).run::<MainWindow>(MainWindowInit {
            idle_monitor_arc: idle_monitor_arc2,
            last_idle_info: idle_info_receiver,
            show_main_window: show_main_window_recv,
        });

        process::exit(0);
    } else {
        println!("Stretch Break is already running, revealing its window");
        Runtime::new()
            .unwrap()
            .block_on(reveal_existing_main_window())
            .ok();
    }
}

async fn reveal_existing_main_window() -> Result<(), Error> {
    let connection = Connection::session().await?;
    let p = Proxy::new(
        &connection,
        format!("{APP_ID}.Core"),
        "/io/github/pieterdd/StretchBreak/Core",
        format!("{APP_ID}.Core"),
    )
    .await?;
    let _: () = p.call("RevealWindow", &()).await?;
    Ok(())
}

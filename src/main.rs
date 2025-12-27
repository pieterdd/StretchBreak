use std::cmp::max;
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
use clap::{ArgAction, Parser, Subcommand, ValueEnum};
use dbus::run_server;
use relm4::RelmApp;
use rodio::{Decoder, OutputStream, Sink};
use single_instance::SingleInstance;
use tokio::sync::watch::{Sender, channel};
use tracing::error;
mod frontend;
use frontend::main_window::{MainWindow, MainWindowInit};
use zbus::{Connection, Error};
mod icons;
use crate::dbus::{DBusAppProxy, WidgetInfo};
use crate::icons::icon_names;

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

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
enum WidgetApiCommand {
    #[value(help = "Outputs time to next break. Empty when break ongoing.")]
    TimeToBreak,
    #[value(
        help = "Outputs time to reset. Empty when user is sufficiently active or just after break finished."
    )]
    TimeToReset,
    #[value(help = "When overdue for a break, outputs how long the user is overdue.")]
    Overtime,
    #[value(help = "Values are 'active', 'snoozed' or 'muted'.")]
    PresenceMode,
}

#[derive(Clone, Copy, Subcommand)]
enum Operation {
    #[command(about = "Stop prompting for breaks for the specified amount of minutes.")]
    SnoozeFor { minutes: i64 },
    #[command(about = "Show break prompts until further notice.")]
    Unmute,
    #[command(about = "Stop prompting for breaks until further notice.")]
    Mute,
    #[command(about = "Start a break right now.")]
    Break,
    #[command(about = "When reading mode is active, timer won't reset during idle activity.")]
    SetReadingMode {
        #[arg(action = ArgAction::Set)]
        value: bool,
    },
    #[command(about = "Status data for desktop widgets that source data from terminal commands.")]
    WidgetApi { command: WidgetApiCommand },
}

#[derive(Parser)]
#[command()]
struct Cli {
    #[arg(long, help = "Background mode: don't show the GUI.")]
    hide: bool,

    #[command(subcommand)]
    operation: Option<Operation>,
}

#[tokio::main]
async fn main() -> zbus::Result<()> {
    #[cfg(target_os = "linux")]
    if libnotify::init(APP_ID).is_err() {
        println!("Warning: could not initialize push notifications");
    }

    let cli = Cli::parse();

    match cli.operation {
        None => start_gui(cli.hide).await,
        Some(operation) => {
            let connection = Connection::session()
                .await
                .expect("Could not create DBus connection");
            let proxy = DBusAppProxy::new(&connection)
                .await
                .expect("Could not open DBus proxy");
            let raw_widget_info = proxy.get_widget_info().await.expect("DBus call failed");
            let widget_info: WidgetInfo =
                serde_json::from_str(&raw_widget_info).expect("Could not load widget info");

            match operation {
                Operation::SnoozeFor { minutes } => {
                    proxy
                        .snooze_for_minutes(max(minutes, 0))
                        .await
                        .expect("Snooze failed");
                }
                Operation::Unmute => {
                    proxy.unmute().await.expect("Unmute failed");
                }
                Operation::Mute => {
                    proxy.mute().await.expect("Mute failed");
                }
                Operation::Break => {
                    proxy.trigger_break().await.expect("Break failed");
                }
                Operation::SetReadingMode { value } => {
                    proxy.set_reading_mode(value).await.expect("Set failed");
                }
                Operation::WidgetApi { command } => {
                    let raw_widget_info = proxy
                        .get_widget_info()
                        .await
                        .expect("Could not retrieve widget info");
                    let widget_info: WidgetInfo = serde_json::from_str(&raw_widget_info)
                        .expect("Could not parse widget info");

                    match command {
                        WidgetApiCommand::TimeToBreak => {
                            print!("{}", widget_info.normal_timer_value);
                        }
                        WidgetApiCommand::TimeToReset => {
                            print!("{}", widget_info.countdown_to_reset_value);
                        }
                        WidgetApiCommand::Overtime => {
                            print!("{}", widget_info.overrun_value);
                        }
                        WidgetApiCommand::PresenceMode => {
                            print!(
                                "{}",
                                match widget_info.presence_mode {
                                    backend::idle_monitoring::PresenceMode::Active => "active",
                                    backend::idle_monitoring::PresenceMode::Muted => "muted",
                                    backend::idle_monitoring::PresenceMode::SnoozedUntil(_) =>
                                        "snoozed",
                                }
                            );
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

async fn start_gui(hide: bool) {
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

        let (show_main_window_sender, show_main_window_recv) = channel(!hide);

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

        relm4_icons::initialize_icons(icon_names::GRESOURCE_BYTES, icon_names::RESOURCE_PREFIX);
        let app = RelmApp::new(APP_ID);
        app.with_args(vec![]).run::<MainWindow>(MainWindowInit {
            idle_monitor_arc: idle_monitor_arc2,
            last_idle_info: idle_info_receiver,
            show_main_window: show_main_window_recv,
        });

        process::exit(0);
    } else {
        println!("Stretch Break is already running, revealing its window");
        reveal_existing_main_window()
            .await
            .expect("Could not communicate with main process");
    }
}

async fn reveal_existing_main_window() -> Result<(), Error> {
    let connection = Connection::session().await?;
    let proxy = DBusAppProxy::new(&connection).await?;
    proxy.reveal_window().await?;
    Ok(())
}

use std::process::Command;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::sleep;
use std::time::Duration;

use crate::backend::idle_monitoring::{
    BREAK_LENGTH_SECS, Clock, DebouncedIdleState, IdleChecker, IdleInfo, IdleMonitor, ModeState,
    REQUIRED_PREBREAK_IDLE_STREAK_SECONDS, TIME_TO_BREAK_SECS,
};
use crate::frontend::formatting::format_timer_timecode;
use adw::prelude::{ActionRowExt, PreferencesRowExt};
use chrono::{Local, TimeDelta, Utc};
use gtk::prelude::{BoxExt, ButtonExt, GtkWindowExt, OrientableExt, ToggleButtonExt, WidgetExt};
use regex::Regex;
use relm4::RelmWidgetExt;
use relm4::prelude::ComponentParts;
use relm4::{Component, ComponentController, ComponentSender, Controller};
use tokio::sync::watch::Receiver;

use super::break_window::{BreakWindow, BreakWindowInit};

pub struct MainWindowInit {
    pub idle_monitor_arc: Arc<Mutex<IdleMonitor<IdleChecker, Clock>>>,
    pub last_idle_info: Receiver<IdleInfo>,
    pub show_main_window: Receiver<bool>,
}

#[derive(Debug)]
pub enum MainWindowMsg {
    Update,
    ForceBreak,
    Mute { minutes: i64 },
    Unmute,
    ToggleReadingMode,
}

#[derive(Debug)]
pub enum MainWindowCmd {
    TriggerUpdate,
}

pub struct MainWindow {
    idle_monitor_arc: Arc<Mutex<IdleMonitor<IdleChecker, Clock>>>,
    previous_mode_state: ModeState,
    last_idle_info_recv: Receiver<IdleInfo>,
    last_idle_info: IdleInfo,
    break_window: Option<Controller<BreakWindow>>,
    show_main_window: Receiver<bool>,
    open_prebreak_notification_id: Option<String>,
}

#[relm4::component(pub)]
impl Component for MainWindow {
    type Init = MainWindowInit;
    type Input = MainWindowMsg;
    type Output = ();
    type CommandOutput = MainWindowCmd;

    view! {
        adw::ApplicationWindow {
            set_title: Some("Stretch Break"),
            set_default_width: 400,
            set_default_height: 200,

            gtk::Box {
                set_orientation: gtk::Orientation::Vertical,

                adw::HeaderBar {},

                gtk::Box {
                    set_orientation: gtk::Orientation::Vertical,
                    set_margin_all: 10,
                    set_spacing: 10,

                    adw::PreferencesGroup {
                        adw::ActionRow {
                            set_title: &format!("Last activity"),
                            #[watch]
                            set_subtitle: &format!("{} seconds ago", model.last_idle_info.idle_since_seconds),
                        },
                    },

                    match model.last_idle_info.last_mode_state {
                        ModeState::Normal { progress_towards_reset, progress_towards_break, idle_state, .. } => {
                            adw::PreferencesGroup {
                                adw::ActionRow {
                                    set_title: &format!("Time to break"),
                                    #[watch]
                                    set_subtitle: &format_timer_timecode(progress_towards_break, TIME_TO_BREAK_SECS),
                                    add_suffix = &gtk::Box {
                                        set_spacing: 5,

                                        gtk::Button {
                                            set_icon_name: "org.gnome.Settings-privacy-symbolic",
                                            set_valign: gtk::Align::Center,
                                            set_tooltip: "Break now",
                                            connect_clicked => MainWindowMsg::ForceBreak,
                                        },
                                        if model.last_idle_info.is_muted() {
                                            gtk::Button {
                                                set_icon_name: "audio-speakers-symbolic",
                                                set_valign: gtk::Align::Center,
                                                set_tooltip: "Unmute",
                                                connect_clicked[sender] => move |_| {
                                                    sender.input(MainWindowMsg::Unmute);
                                                },
                                            }
                                        } else {
                                            gtk::Button {
                                                set_icon_name: "audio-volume-muted-symbolic",
                                                set_valign: gtk::Align::Center,
                                                set_tooltip: "Mute for 30 minutes",
                                                connect_clicked[sender] => move |_| {
                                                    sender.input(MainWindowMsg::Mute { minutes: 30 });
                                                },
                                            }
                                        },

                                        gtk::ToggleButton {
                                            set_icon_name: "x-office-document-symbolic",
                                            set_valign: gtk::Align::Center,
                                            #[watch]
                                            set_active: model.last_idle_info.reading_mode,
                                            set_tooltip: "Reading mode",
                                            connect_clicked => MainWindowMsg::ToggleReadingMode,
                                        }
                                    }
                                },

                                adw::ActionRow {
                                    set_title: &format!("Time to reset"),
                                    #[watch]
                                    set_subtitle: &format_timer_timecode(progress_towards_reset, BREAK_LENGTH_SECS),
                                },

                                adw::ActionRow {
                                    set_title: &format!("Activity state"),
                                    #[watch]
                                    set_subtitle: match idle_state {
                                        DebouncedIdleState::Active { active_since } => format!("Active for {} seconds", Local::now().signed_duration_since(active_since).num_seconds()),
                                        DebouncedIdleState::IdleGoingToActive { .. } => format!("Idle going to active"),
                                        DebouncedIdleState::ActiveGoingToIdle { .. } => format!("Active going to idle"),
                                        DebouncedIdleState::Idle { idle_since } => format!("Idle for {} seconds", Local::now().signed_duration_since(idle_since).num_seconds()),
                                    }.as_str(),
                                },
                            }
                        },
                        ModeState::Break { progress_towards_finish, idle_state } => {
                            adw::PreferencesGroup {
                                adw::ActionRow {
                                    set_title: &format!("Break remainder"),
                                    #[watch]
                                    set_subtitle: &format_timer_timecode(progress_towards_finish, BREAK_LENGTH_SECS),
                                },

                                adw::ActionRow {
                                    set_title: &format!("State"),
                                    #[watch]
                                    set_subtitle: match idle_state {
                                        DebouncedIdleState::Active { active_since } => format!("Active for {} seconds", Local::now().signed_duration_since(active_since).num_seconds()),
                                        DebouncedIdleState::IdleGoingToActive { .. } => format!("Idle going to active"),
                                        DebouncedIdleState::ActiveGoingToIdle { .. } => format!("Active going to idle"),
                                        DebouncedIdleState::Idle { idle_since } => format!("Idle for {} seconds", Local::now().signed_duration_since(idle_since).num_seconds()),
                                    }.as_str(),
                                },
                            }
                        }
                        ModeState::PreBreak { .. } => {
                            adw::PreferencesGroup {
                                adw::ActionRow {
                                    set_title: &format!("Prebreak"),
                                    #[watch]
                                    set_subtitle: &format!("{} seconds to break", REQUIRED_PREBREAK_IDLE_STREAK_SECONDS - model.last_idle_info.idle_since_seconds),
                                },
                            }
                        }
                    }
                }
            }
        }
    }

    fn init(
        init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let previous_last_idle_info = *init.last_idle_info.borrow();
        let model = MainWindow {
            idle_monitor_arc: init.idle_monitor_arc,
            previous_mode_state: previous_last_idle_info.last_mode_state,
            last_idle_info_recv: init.last_idle_info,
            last_idle_info: previous_last_idle_info,
            break_window: None,
            show_main_window: init.show_main_window,
            open_prebreak_notification_id: None,
        };
        let widgets = view_output!();

        sender.input(MainWindowMsg::Update);
        ComponentParts { model, widgets }
    }

    fn update(
        &mut self,
        message: Self::Input,
        sender: ComponentSender<Self>,
        root: &Self::Root,
    ) -> () {
        match message {
            MainWindowMsg::Update => {
                sender.spawn_oneshot_command(|| {
                    sleep(Duration::from_millis(100));
                    MainWindowCmd::TriggerUpdate
                });
                match self.last_idle_info.last_mode_state {
                    ModeState::Normal { .. } => {}
                    ModeState::PreBreak { .. } => match self.previous_mode_state {
                        ModeState::PreBreak { .. } => {}
                        _ => {
                            // Try to warn about prebreak if notify-send is installed
                            match Command::new("gdbus")
                                .args([
                                    "call",
                                    "--session",
                                    "--dest",
                                    "org.freedesktop.Notifications",
                                    "--object-path",
                                    "/org/freedesktop/Notifications",
                                    "--method",
                                    "org.freedesktop.Notifications.Notify",
                                    "Stretch Break",
                                    "0",
                                    "io.github.pieterdd.StretchBreak",
                                    "Time to stretch",
                                    "Break will start when mouse and keyboard are released.",
                                    "[]",
                                    "{\"urgency\": <int32 2>}",
                                    "0",
                                ])
                                .output()
                            {
                                Ok(out) => {
                                    let str_output = String::from_utf8(out.stdout).unwrap();
                                    let re = Regex::new(r"\(uint32 ([0-9]+),\)").unwrap();

                                    if let Some(captures) = re.captures(&str_output) {
                                        if let Some(number) = captures.get(1) {
                                            self.open_prebreak_notification_id =
                                                Some(String::from(number.as_str()));
                                        }
                                    }
                                }
                                Err(_) => {}
                            }
                        }
                    },
                    ModeState::Break { .. } => match self.previous_mode_state {
                        ModeState::Break { .. } => {}
                        _ => {
                            if let Some(notification_id) = &self.open_prebreak_notification_id {
                                Command::new("gdbus")
                                    .args([
                                        "call",
                                        "--session",
                                        "--dest",
                                        "org.freedesktop.Notifications",
                                        "--object-path",
                                        "/org/freedesktop/Notifications",
                                        "--method",
                                        "org.freedesktop.Notifications.CloseNotification",
                                        notification_id,
                                    ])
                                    .output()
                                    .ok();
                                self.open_prebreak_notification_id = None;
                            }
                            let break_window_init = BreakWindowInit {
                                idle_monitor_arc: self.idle_monitor_arc.clone(),
                                last_idle_info_recv: self.last_idle_info_recv.clone(),
                            };
                            let break_window =
                                BreakWindow::builder().launch(break_window_init).detach();
                            break_window.widget().present();
                            self.break_window = Some(break_window);
                        }
                    },
                }
                self.previous_mode_state = self.last_idle_info.last_mode_state;
                let visible = *self.show_main_window.borrow();
                root.set_visible(visible);
            }
            MainWindowMsg::ForceBreak => {
                self._unwrapped_idle_monitor().force_break();
            }
            MainWindowMsg::Mute { minutes } => {
                let unmute_timestamp = Utc::now()
                    .checked_add_signed(TimeDelta::minutes(minutes))
                    .unwrap();
                self._unwrapped_idle_monitor().mute_until(unmute_timestamp);
            }
            MainWindowMsg::Unmute => {
                self._unwrapped_idle_monitor().unmute();
            }
            MainWindowMsg::ToggleReadingMode => {
                self._unwrapped_idle_monitor()
                    .set_reading_mode(!self.last_idle_info.reading_mode);
            }
        }
    }

    fn update_cmd(
        &mut self,
        message: Self::CommandOutput,
        sender: ComponentSender<Self>,
        _root: &Self::Root,
    ) {
        match message {
            Self::CommandOutput::TriggerUpdate => {
                self.last_idle_info = self.last_idle_info_recv.borrow().clone();
                sender.input(MainWindowMsg::Update);
            }
        }
    }
}

impl MainWindow {
    fn _unwrapped_idle_monitor(&self) -> MutexGuard<'_, IdleMonitor<IdleChecker, Clock>> {
        self.idle_monitor_arc
            .lock()
            .expect("Unable to obtain idle monitor lock")
    }
}

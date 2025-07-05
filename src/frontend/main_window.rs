use std::cmp::min;
use std::process;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::sleep;
use std::time::Duration;

use crate::APP_ID;
use crate::backend::idle_monitoring::{
    Clock, DebouncedIdleState, IdleChecker, IdleInfo, IdleMonitor, ModeState, PresenceMode,
    REQUIRED_PREBREAK_IDLE_STREAK_SECONDS,
};
use crate::frontend::formatting::format_timer_timecode;
use adw::prelude::{ActionRowExt, AdwDialogExt, PreferencesRowExt};
use chrono::{DateTime, Local, TimeDelta, Utc};
use gtk::prelude::{BoxExt, ButtonExt, GtkWindowExt, OrientableExt, WidgetExt};
use libnotify::{Notification, Urgency};
use relm4::RelmWidgetExt;
use relm4::actions::{RelmAction, RelmActionGroup};
use relm4::prelude::ComponentParts;
use relm4::{Component, ComponentController, ComponentSender, Controller};
use relm4_icons::icon_names;
use tokio::sync::watch::Receiver;

use super::break_window::{BreakWindow, BreakWindowInit};

relm4::new_action_group!(TopNavActionGroup, "top_nav");
relm4::new_stateless_action!(AboutAction, TopNavActionGroup, "about");
relm4::new_stateless_action!(QuitAction, TopNavActionGroup, "quit");

relm4::new_action_group!(SnoozeActionGroup, "snooze");
relm4::new_stateless_action!(Snooze30mAction, SnoozeActionGroup, "snooze_30m");
relm4::new_stateless_action!(Snooze1hAction, SnoozeActionGroup, "snooze_1h");
relm4::new_stateless_action!(Snooze3hAction, SnoozeActionGroup, "snooze_6h");

pub struct MainWindowInit {
    pub idle_monitor_arc: Arc<Mutex<IdleMonitor<IdleChecker, Clock>>>,
    pub last_idle_info: Receiver<IdleInfo>,
    pub show_main_window: Receiver<bool>,
}

#[derive(Debug)]
pub enum MainWindowMsg {
    Update,
    ForceBreak,
    Snooze { minutes: i64 },
    Mute,
    Unmute,
    SetReadingMode(bool),
    SetTimeToBreak(i64),
    SetBreakLength(i64),
    Hide { notify: bool },
}

#[derive(Debug)]
pub enum MainWindowCmd {
    TriggerUpdate,
}

pub struct MainWindow {
    idle_monitor_arc: Arc<Mutex<IdleMonitor<IdleChecker, Clock>>>,
    previous_mode_state: ModeState,
    last_idle_info: IdleInfo,
    break_window: Option<Controller<BreakWindow>>,
    show_main_window: Receiver<bool>,
    prebreak_notification: Option<Notification>,
    time_to_break_secs: i64,
}

#[relm4::component(pub)]
impl Component for MainWindow {
    type Init = MainWindowInit;
    type Input = MainWindowMsg;
    type Output = ();
    type CommandOutput = MainWindowCmd;

    menu! {
        top_nav: {
            section! {
                "About" => AboutAction,
                "Quit" => QuitAction,
            }
        },
        snooze: {
            section! {
                "30 minutes" => Snooze30mAction,
                "1 hour" => Snooze1hAction,
                "3 hours" => Snooze3hAction,
            }
        }
    }

    view! {
        main_window = &adw::ApplicationWindow {
            set_title: Some("Stretch Break"),
            set_default_width: 400,
            set_default_height: 200,
            connect_close_request[sender] => move |_| {
                sender.input(MainWindowMsg::Hide { notify: true });
                glib::Propagation::Stop
            },

            adw::ToolbarView {
                add_top_bar = &adw::HeaderBar {
                    pack_start = &gtk::MenuButton {
                        set_icon_name: "open-menu-symbolic",
                        set_menu_model: Some(&top_nav)
                    }
                },

                #[wrap(Some)]
                set_content = &gtk::Box {
                    set_orientation: gtk::Orientation::Vertical,

                    adw::ViewSwitcher {
                        set_margin_horizontal: 10,
                        set_policy: adw::ViewSwitcherPolicy::Wide,
                        set_stack: Some(&view_stack),
                    },

                    #[name = "view_stack"]
                    adw::ViewStack {
                        add_titled_with_icon[Some("status"), "Status", icon_names::STOPWATCH] = &gtk::Box {
                            set_orientation: gtk::Orientation::Vertical,
                            set_margin_all: 10,
                            set_spacing: 10,

                            match model.last_idle_info.last_mode_state {
                                ModeState::Normal { progress_towards_reset, progress_towards_break, idle_state, .. } => {
                                    adw::PreferencesGroup {
                                        adw::ActionRow {
                                            set_title: &format!("Time to break"),
                                            #[watch]
                                            set_subtitle: &format_timer_timecode(progress_towards_break, model.last_idle_info.time_to_break_secs),
                                            add_suffix = &gtk::Box {
                                                set_spacing: 5,

                                                gtk::Button {
                                                    set_icon_name: "org.gnome.Settings-privacy-symbolic",
                                                    set_valign: gtk::Align::Center,
                                                    set_tooltip: "Take break now",
                                                    connect_clicked => MainWindowMsg::ForceBreak,
                                                },
                                            }
                                        },

                                        adw::ActionRow {
                                            set_title: &format!("Time to reset"),
                                            #[watch]
                                            set_subtitle: &format_timer_timecode(progress_towards_reset, model.last_idle_info.break_length_secs),
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
                                            set_subtitle: &format_timer_timecode(progress_towards_finish, model.last_idle_info.break_length_secs),
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
                                            set_subtitle: &format!("{} seconds to break", REQUIRED_PREBREAK_IDLE_STREAK_SECONDS - min(model.last_idle_info.idle_since_seconds, REQUIRED_PREBREAK_IDLE_STREAK_SECONDS)),
                                        },
                                    }
                                }
                            },

                            adw::PreferencesGroup {
                                adw::ActionRow {
                                    set_title: &format!("Last activity"),
                                    #[watch]
                                    set_subtitle: &format!("{} seconds ago", model.last_idle_info.idle_since_seconds),
                                },
                            },
                        },

                        add_titled_with_icon[Some("settings"), "Settings", icon_names::SETTINGS] = &gtk::Box {
                            set_orientation: gtk::Orientation::Vertical,
                            set_margin_all: 10,
                            set_spacing: 10,

                            adw::PreferencesGroup {
                                adw::SwitchRow {
                                    set_title: &format!("Reading mode"),
                                    #[watch]
                                    set_subtitle: match model.last_idle_info.reading_mode {
                                        true => "Break timer will not reset while idle",
                                        false => "Break timer may reset while idle",
                                    },
                                    #[watch]
                                    set_active: model.last_idle_info.reading_mode,
                                    connect_active_notify[sender] => move |switch| {
                                        sender.input(MainWindowMsg::SetReadingMode(switch.is_active()));
                                    }
                                },
                                adw::ActionRow {
                                    set_title: &format!("Break notifications"),
                                    #[watch]
                                    set_subtitle: &match model.last_idle_info.presence_mode {
                                        PresenceMode::Active => format!("Enabled"),
                                        PresenceMode::SnoozedUntil(timestamp) => format!("Snoozed until {}", DateTime::<Local>::from(timestamp).format("%R")),
                                        PresenceMode::Muted => format!("Muted"),
                                    },
                                    add_suffix = &gtk::Box {
                                        set_valign: gtk::Align::Center,
                                        set_spacing: 5,

                                        if model.last_idle_info.is_muted() {
                                            gtk::Button {
                                                set_icon_name: "audio-speakers-symbolic",
                                                set_halign: gtk::Align::End,
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
                                                set_tooltip: "Mute",
                                                connect_clicked[sender] => move |_| {
                                                    sender.input(MainWindowMsg::Mute);
                                                },
                                            }
                                        },
                                        #[local_ref]
                                        snooze_button -> gtk::MenuButton {
                                            set_icon_name: "snooze-filled",
                                            set_direction: gtk::ArrowType::Down,
                                            set_menu_model: Some(&snooze),
                                        }
                                    }
                                },
                                adw::SpinRow {
                                    set_title: "Time between breaks",
                                    set_subtitle: "In minutes",
                                    #[block_signal(time_to_break_handler)]
                                    set_adjustment: Some(&gtk::Adjustment::new(
                                        model.time_to_break_secs as f64 / 60.0,
                                        0.0, 1440.0, 1.0, 1.0, 0.0,
                                    )),
                                    set_snap_to_ticks: false,
                                    connect_value_notify[sender] => move |row| {
                                        sender.input(MainWindowMsg::SetTimeToBreak(row.value().round() as i64))
                                    } @time_to_break_handler
                                },
                                adw::SpinRow {
                                    set_title: "Break length",
                                    set_subtitle: "In seconds",
                                    #[block_signal(break_length_handler)]
                                    set_adjustment: Some(&gtk::Adjustment::new(
                                        model.last_idle_info.break_length_secs as f64,
                                        0.0, 86400.0, 10.0, 1.0, 0.0,
                                    )),
                                    set_snap_to_ticks: false,
                                    connect_value_notify[sender] => move |row| {
                                        sender.input(MainWindowMsg::SetBreakLength(row.value().round() as i64))
                                    } @break_length_handler
                                }
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
        let visible = *init.show_main_window.borrow();
        if !visible {
            sender.input(MainWindowMsg::Hide { notify: false });
        }

        let model = MainWindow {
            idle_monitor_arc: init.idle_monitor_arc,
            previous_mode_state: previous_last_idle_info.last_mode_state,
            last_idle_info: previous_last_idle_info,
            break_window: None,
            show_main_window: init.show_main_window,
            prebreak_notification: None,
            time_to_break_secs: previous_last_idle_info.time_to_break_secs,
        };
        let snooze_button = gtk::MenuButton::builder().build();
        let widgets = view_output!();

        let mut top_nav_group = RelmActionGroup::<TopNavActionGroup>::new();
        let cloned_root = root.clone();
        let about: RelmAction<AboutAction> = RelmAction::new_stateless(move |_| {
            let dialog = adw::AboutDialog::builder()
                .application_name("Stretch Break")
                .application_icon(APP_ID)
                .developer_name("pieterdd")
                .version(env!("CARGO_PKG_VERSION"))
                .website("https://github.com/pieterdd/StretchBreak/")
                .build();
            dialog.present(Some(&cloned_root));
        });
        top_nav_group.add_action(about);
        let quit: RelmAction<QuitAction> = RelmAction::new_stateless(move |_| {
            process::exit(0);
        });
        top_nav_group.add_action(quit);
        let top_nav_actions = top_nav_group.into_action_group();
        widgets
            .main_window
            .insert_action_group("top_nav", Some(&top_nav_actions));

        let mut snooze_group = RelmActionGroup::<SnoozeActionGroup>::new();
        let sender_copy1 = sender.clone();
        let snooze_30m: RelmAction<Snooze30mAction> = RelmAction::new_stateless(move |_| {
            sender_copy1.input(MainWindowMsg::Snooze { minutes: 30 });
        });
        snooze_group.add_action(snooze_30m);
        let sender_copy2 = sender.clone();
        let snooze_1h: RelmAction<Snooze1hAction> = RelmAction::new_stateless(move |_| {
            sender_copy2.input(MainWindowMsg::Snooze { minutes: 60 });
        });
        snooze_group.add_action(snooze_1h);
        let sender_copy3 = sender.clone();
        let snooze_3h: RelmAction<Snooze3hAction> = RelmAction::new_stateless(move |_| {
            sender_copy3.input(MainWindowMsg::Snooze { minutes: 60 * 3 });
        });
        snooze_group.add_action(snooze_3h);
        let snooze_actions = snooze_group.into_action_group();
        widgets
            .snooze_button
            .insert_action_group("snooze", Some(&snooze_actions));

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
                #[cfg(target_os = "linux")]
                if let Some(notification) = &self.prebreak_notification {
                    if let ModeState::PreBreak { .. } = self.last_idle_info.last_mode_state {
                    } else {
                        if let Err(_) = notification.close() {
                            println!("Warning: failed to close notification");
                        }
                        self.prebreak_notification = None;
                    }
                }
                match self.last_idle_info.last_mode_state {
                    ModeState::Normal { .. } => {}
                    ModeState::PreBreak { .. } => {
                        match self.previous_mode_state {
                            ModeState::PreBreak { .. } => {}
                            _ => {
                                // Try to warn about prebreak if notify-send is installed
                                #[cfg(target_os = "linux")]
                                {
                                    let prebreak_notification = Notification::new(
                                        "Time to stretch",
                                        "Break will start when mouse and keyboard are released.",
                                        None,
                                    );
                                    prebreak_notification.set_urgency(Urgency::Critical);
                                    prebreak_notification.show().ok();
                                    self.prebreak_notification = Some(prebreak_notification);
                                }
                            }
                        }
                    }
                    ModeState::Break { .. } => match self.previous_mode_state {
                        ModeState::Break { .. } => {}
                        _ => {
                            let break_window_init = BreakWindowInit {
                                idle_monitor_arc: self.idle_monitor_arc.clone(),
                            };
                            let break_window =
                                BreakWindow::builder().launch(break_window_init).detach();
                            break_window.widget().present();
                            self.break_window = Some(break_window);
                        }
                    },
                }
                self.previous_mode_state = self.last_idle_info.last_mode_state;
                if self.show_main_window.has_changed().unwrap() {
                    let visible = *self.show_main_window.borrow_and_update();
                    root.set_visible(visible);
                }
                if self.time_to_break_secs != self.last_idle_info.time_to_break_secs {
                    self.time_to_break_secs = self.last_idle_info.time_to_break_secs;
                }
            }
            MainWindowMsg::ForceBreak => {
                self._unwrapped_idle_monitor().trigger_break();
            }
            MainWindowMsg::Snooze { minutes } => {
                let unmute_timestamp = Utc::now()
                    .checked_add_signed(TimeDelta::minutes(minutes))
                    .unwrap();
                self._unwrapped_idle_monitor().snooze(unmute_timestamp);
            }
            MainWindowMsg::Mute => {
                self._unwrapped_idle_monitor().mute();
            }
            MainWindowMsg::Unmute => {
                self._unwrapped_idle_monitor().unmute();
            }
            MainWindowMsg::SetReadingMode(value) => {
                self._unwrapped_idle_monitor().set_reading_mode(value);
                self.last_idle_info = self.idle_monitor_arc.lock().unwrap().get_last_idle_info();
            }
            MainWindowMsg::SetTimeToBreak(value) => {
                if self.last_idle_info.time_to_break_secs != value {
                    self._unwrapped_idle_monitor().set_time_to_break(value * 60);
                }
            }
            MainWindowMsg::SetBreakLength(value) => {
                if self.last_idle_info.break_length_secs != value {
                    self._unwrapped_idle_monitor().set_break_length(value);
                }
            }
            MainWindowMsg::Hide { notify } => {
                root.set_visible(false);
                #[cfg(target_os = "linux")]
                if notify {
                    let notification = Notification::new(
                        "Still here!",
                        "Stretch Break continues running in the background.",
                        None,
                    );
                    notification.set_timeout(3_000);
                    notification.show().ok();
                }
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
                self.last_idle_info = self.idle_monitor_arc.lock().unwrap().get_last_idle_info();
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

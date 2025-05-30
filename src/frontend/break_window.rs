use relm4_icons::icon_names;
use std::sync::{Arc, Mutex};
use std::thread::sleep;
use std::time::Duration;

use crate::backend::idle_monitoring::{
    BREAK_LENGTH_SECS, Clock, IdleChecker, IdleInfo, IdleMonitor, ModeState,
};
use chrono::TimeDelta;
use gtk::prelude::{BoxExt, ButtonExt, GtkWindowExt, OrientableExt, WidgetExt};
use relm4::{Component, ComponentParts};
use relm4::{ComponentSender, RelmWidgetExt};
use tokio::sync::watch::Receiver;

pub struct BreakWindowInit {
    pub idle_monitor_arc: Arc<Mutex<IdleMonitor<IdleChecker, Clock>>>,
    pub last_idle_info_recv: Receiver<IdleInfo>,
}

#[derive(Debug)]
pub enum BreakWindowCmd {
    Update,
}

#[derive(Debug)]
pub enum BreakWindowMsg {
    Update,
    Postpone,
    Skip,
}

pub struct BreakWindow {
    idle_monitor_arc: Arc<Mutex<IdleMonitor<IdleChecker, Clock>>>,
    last_idle_info_recv: Receiver<IdleInfo>,
    last_idle_info: IdleInfo,
    user_is_active: bool,
}

#[relm4::component(pub)]
impl Component for BreakWindow {
    type Init = BreakWindowInit;
    type Input = BreakWindowMsg;
    type Output = ();
    type CommandOutput = BreakWindowCmd;

    view! {
        adw::Window {
            set_title: Some("Stretch Break"),
            set_default_width: 600,
            set_default_height: 300,
            set_resizable: false,
            set_deletable: false,

            gtk::Box {
                set_orientation: gtk::Orientation::Horizontal,
                set_halign: gtk::Align::Center,
                set_spacing: 30,

                gtk::Image {
                    set_icon_name: Some(icon_names::TIMER),
                    set_pixel_size: 80,
                },

                gtk::Box {
                    set_orientation: gtk::Orientation::Vertical,
                    set_margin_all: 10,
                    set_spacing: 15,
                    set_vexpand: true,
                    set_valign: gtk::Align::Center,

                    if model.user_is_active {
                        gtk::Label {
                            set_markup: "<big>Break continues when idle</big>",
                            set_halign: gtk::Align::Start,
                        }
                    } else {
                        gtk::Label {
                            #[watch]
                            set_markup: &format!("<big>Breaking for {} seconds</big>", match model.last_idle_info.last_mode_state {
                                ModeState::Break { progress_towards_finish, .. } => {
                                    BREAK_LENGTH_SECS - progress_towards_finish.num_seconds()
                                },
                                _ => 0
                            }),
                            set_halign: gtk::Align::Start,
                        }
                    },

                    gtk::Box {
                        set_orientation: gtk::Orientation::Horizontal,
                        set_spacing: 10,
                        set_halign: gtk::Align::Start,

                        gtk::Button {
                            set_label: "Postpone",
                            connect_clicked => BreakWindowMsg::Postpone,
                        },

                        gtk::Button {
                            set_label: "Skip",
                            connect_clicked => BreakWindowMsg::Skip,
                        },
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
        let last_idle_info = init.last_idle_info_recv.borrow().clone();
        let model = BreakWindow {
            idle_monitor_arc: init.idle_monitor_arc,
            last_idle_info_recv: init.last_idle_info_recv,
            last_idle_info: last_idle_info,
            user_is_active: false,
        };
        let widgets = view_output!();

        sender.input(BreakWindowMsg::Update);
        ComponentParts { model, widgets }
    }

    fn update(
        &mut self,
        message: Self::Input,
        sender: ComponentSender<Self>,
        root: &Self::Root,
    ) -> () {
        match message {
            BreakWindowMsg::Update => {
                sender.spawn_oneshot_command(|| {
                    sleep(Duration::from_millis(100));
                    BreakWindowCmd::Update
                });
                match self.last_idle_info.last_mode_state {
                    ModeState::Break {
                        progress_towards_finish,
                        ..
                    } => {
                        if progress_towards_finish.num_seconds() == BREAK_LENGTH_SECS {
                            root.close();
                        }
                    }
                    _ => {
                        root.close();
                    }
                }
            }
            BreakWindowMsg::Postpone => {
                self.idle_monitor_arc
                    .lock()
                    .unwrap()
                    .postpone_break(TimeDelta::minutes(1));
            }
            BreakWindowMsg::Skip => {
                self.idle_monitor_arc.lock().unwrap().skip_break();
            }
        }
    }

    fn update_cmd(
        &mut self,
        _message: Self::CommandOutput,
        sender: ComponentSender<Self>,
        _root: &Self::Root,
    ) {
        self.last_idle_info = self.last_idle_info_recv.borrow().clone();
        if let ModeState::Break { idle_state, .. } = self.last_idle_info.last_mode_state {
            self.user_is_active = idle_state.is_user_active();
        }
        sender.input(BreakWindowMsg::Update);
    }
}

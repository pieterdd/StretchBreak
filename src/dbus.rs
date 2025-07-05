use std::{
    error::Error,
    sync::{Arc, Mutex, MutexGuard},
};
use zbus::object_server::SignalEmitter;

use chrono::{DateTime, Duration, Local, TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::watch::{Receiver, Sender};
use zbus::{connection, interface};

use crate::{
    backend::idle_monitoring::{
        Clock, DebouncedIdleState, IdleChecker, IdleInfo, IdleMonitor, ModeState, PresenceMode,
    },
    frontend::formatting::{format_timedelta_timecode, format_timer_timecode},
};

#[derive(Serialize, Deserialize, PartialEq, Eq, Debug)]
struct WidgetInfo {
    normal_timer_value: String,
    prebreak_timer_value: String, // Deprecrated - remove in 0.1.9
    overrun_value: String,
    presence_mode: PresenceMode,
    snoozed_until_time: Option<String>,
    reading_mode: bool,
}

fn get_widget_info(idle_info: &IdleInfo) -> WidgetInfo {
    let overrun_value = if idle_info.overrun == Duration::seconds(0) {
        String::from("")
    } else {
        match idle_info.last_mode_state {
            ModeState::Break { idle_state, .. } => match idle_state {
                DebouncedIdleState::Idle { .. } | DebouncedIdleState::IdleGoingToActive { .. } => {
                    String::from("")
                }
                _ => format_timedelta_timecode(&idle_info.overrun),
            },
            _ => format_timedelta_timecode(&idle_info.overrun),
        }
    };
    WidgetInfo {
        normal_timer_value: match idle_info.last_mode_state {
            ModeState::Normal {
                progress_towards_break,
                ..
            } if progress_towards_break.num_seconds() < idle_info.time_to_break_secs => {
                format_timer_timecode(progress_towards_break, idle_info.time_to_break_secs)
            }
            _ => String::from(""),
        },
        // This field is repurposed for backwards compatibility - remove in 0.1.9
        prebreak_timer_value: overrun_value.clone(),
        overrun_value: overrun_value.clone(),
        presence_mode: idle_info.presence_mode,
        snoozed_until_time: match idle_info.presence_mode {
            PresenceMode::Active => None,
            PresenceMode::SnoozedUntil(timestamp) => Some(format!(
                "{}",
                DateTime::<Local>::from(timestamp).format("%R")
            )),
            PresenceMode::Muted => None,
        },
        reading_mode: idle_info.reading_mode,
    }
}

#[tokio::main]
pub async fn run_server(
    mut idle_info_recv: Receiver<IdleInfo>,
    show_main_window_send: Sender<bool>,
    idle_monitor_arc: Arc<Mutex<IdleMonitor<IdleChecker, Clock>>>,
) -> Result<(), Box<dyn Error>> {
    let conn = connection::Builder::session()?
        .name("io.github.pieterdd.StretchBreak.Core")?
        .serve_at(
            "/io/github/pieterdd/StretchBreak/Core",
            DBusServer {
                show_main_window_send,
                idle_monitor_arc,
            },
        )?
        .build()
        .await?;

    loop {
        let idle_info = *idle_info_recv.borrow_and_update();
        let serialized_idle_info = serde_json::to_string(&get_widget_info(&idle_info))
            .expect("Serde JSON conversion failed");
        conn.object_server()
            .interface("/io/github/pieterdd/StretchBreak/Core")
            .await?
            .widget_info_updated(serialized_idle_info)
            .await?;

        idle_info_recv.changed().await?;
    }
}

struct DBusServer {
    show_main_window_send: Sender<bool>,
    idle_monitor_arc: Arc<Mutex<IdleMonitor<IdleChecker, Clock>>>,
}

impl DBusServer {
    fn _unlock_monitor(&self) -> MutexGuard<'_, IdleMonitor<IdleChecker, Clock>> {
        return self
            .idle_monitor_arc
            .lock()
            .expect("Unlocking idle monitor failed");
    }
}

#[interface(name = "io.github.pieterdd.StretchBreak.Core", proxy())]
impl DBusServer {
    #[zbus(signal)]
    async fn widget_info_updated(
        signal_emitter: &SignalEmitter<'_>,
        serialized_idle_info: String,
    ) -> zbus::Result<()>;

    fn toggle_window(&self) {
        // Deprecated - remove in 0.1.7
        self.reveal_window();
    }

    fn reveal_window(&self) {
        self.show_main_window_send.send(true).expect("Send failed");
    }

    fn mute(&self) {
        let mut monitor = self._unlock_monitor();
        monitor.mute();
    }

    fn snooze_for_minutes(&self, num_minutes: i64) {
        let unmute_time = Utc::now()
            .checked_add_signed(TimeDelta::minutes(num_minutes))
            .unwrap();
        let mut monitor = self._unlock_monitor();
        monitor.snooze(unmute_time);
    }

    fn unmute(&self) {
        let mut monitor = self._unlock_monitor();
        monitor.unmute();
    }

    fn set_reading_mode(&self, value: bool) {
        let mut monitor = self._unlock_monitor();
        monitor.set_reading_mode(value);
    }

    fn trigger_break(&self) {
        let mut monitor = self._unlock_monitor();
        monitor.trigger_break();
    }
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, Local, TimeDelta, TimeZone, Utc};

    use crate::{
        backend::idle_monitoring::{
            DEFAULT_BREAK_LENGTH_SECS, DEFAULT_TIME_TO_BREAK_SECS, DebouncedIdleState, IdleInfo,
            ModeState, PresenceMode,
        },
        dbus::{WidgetInfo, get_widget_info},
    };

    #[test]
    fn idle_status_normal() {
        let now = Local::now().to_utc();
        let info = IdleInfo {
            idle_since_seconds: 2,
            last_checked: now,
            last_mode_state: ModeState::Normal {
                progress_towards_break: TimeDelta::seconds(31),
                progress_towards_reset: TimeDelta::seconds(2),
                idle_state: DebouncedIdleState::Active {
                    active_since: now.checked_sub_signed(TimeDelta::seconds(20)).unwrap(),
                },
            },
            presence_mode: PresenceMode::Active,
            reading_mode: false,
            time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
            break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
            overrun: Duration::milliseconds(0_000),
        };
        assert_eq!(
            get_widget_info(&info),
            WidgetInfo {
                normal_timer_value: String::from("19:29"),
                prebreak_timer_value: String::from(""),
                overrun_value: String::from(""),
                presence_mode: PresenceMode::Active,
                snoozed_until_time: None,
                reading_mode: false,
            }
        )
    }

    #[test]
    fn with_overrun() {
        let now = Local::now().to_utc();
        let info = IdleInfo {
            idle_since_seconds: 2,
            last_checked: now,
            last_mode_state: ModeState::PreBreak {
                started_at: now - Duration::seconds(1),
            },
            presence_mode: PresenceMode::Active,
            reading_mode: false,
            time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
            break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
            overrun: Duration::milliseconds(1_000),
        };
        assert_eq!(
            get_widget_info(&info),
            WidgetInfo {
                normal_timer_value: String::from(""),
                prebreak_timer_value: String::from("0:01"),
                overrun_value: String::from("0:01"),
                presence_mode: PresenceMode::Active,
                snoozed_until_time: None,
                reading_mode: false,
            }
        )
    }

    #[test]
    fn idle_status_muted() {
        let now = Local::now().to_utc();
        let info = IdleInfo {
            idle_since_seconds: 2,
            last_checked: now,
            last_mode_state: ModeState::Normal {
                progress_towards_break: TimeDelta::seconds(31),
                progress_towards_reset: TimeDelta::seconds(2),
                idle_state: DebouncedIdleState::Active {
                    active_since: now.checked_sub_signed(TimeDelta::seconds(20)).unwrap(),
                },
            },
            presence_mode: PresenceMode::SnoozedUntil(
                Utc.with_ymd_and_hms(2025, 2, 3, 12, 34, 11).unwrap(),
            ),
            reading_mode: false,
            time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
            break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
            overrun: Duration::milliseconds(0_000),
        };
        assert_eq!(
            get_widget_info(&info),
            WidgetInfo {
                normal_timer_value: String::from("19:29"),
                prebreak_timer_value: String::from(""),
                overrun_value: String::from(""),
                presence_mode: PresenceMode::SnoozedUntil(
                    Utc.with_ymd_and_hms(2025, 2, 3, 12, 34, 11).unwrap(),
                ),
                snoozed_until_time: Some(String::from("13:34")),
                reading_mode: false,
            }
        )
    }
}

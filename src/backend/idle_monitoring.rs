use chrono::{DateTime, Duration, Utc};
use core::fmt;
use serde::{Deserialize, Serialize};
use std::cmp::min;
use user_idle2::UserIdle;

#[cfg(test)]
use mockall::automock;

use crate::backend::file_io::PersistableState;

pub const DEFAULT_TIME_TO_BREAK_SECS: i64 = 20 * 60;
pub const DEFAULT_BREAK_LENGTH_SECS: i64 = 90;
pub const REQUIRED_PREBREAK_IDLE_STREAK_SECONDS: u64 = 5;
const FRAME_DROP_CUTOFF_POINT_SECS: i64 = 30;

const TRANSITION_THRESHOLD_SECS: u64 = 3;

// Indicates max. number of seconds that may have elapsed since the last activity
// for that activity to qualify an IdleGoingToActive -> Active transition.
const END_OF_ACTIVE_PUSHING_TRANSITION_WINDOW: u64 = TRANSITION_THRESHOLD_SECS - 2;

pub trait AbstractIdleChecker {
    fn get_idle_time_in_seconds(&self) -> u64;
}

pub struct IdleChecker;
#[cfg_attr(test, automock)]
impl AbstractIdleChecker for IdleChecker {
    fn get_idle_time_in_seconds(&self) -> u64 {
        match UserIdle::get_time() {
            Ok(time) => time.as_seconds(),
            Err(_) => {
                println!("Could not get idle time. Faking value until available again.");
                1
            }
        }
    }
}

pub trait AbstractClock {
    fn get_time(&self) -> DateTime<Utc>;
}

pub struct Clock;
#[cfg_attr(test, automock)]
impl AbstractClock for Clock {
    fn get_time(&self) -> DateTime<Utc> {
        return Utc::now();
    }
}

#[derive(Debug, PartialEq, Clone, Copy, Serialize, Deserialize)]
#[serde(tag = "state")]
pub enum DebouncedIdleState {
    Idle {
        idle_since: DateTime<Utc>,
    },
    IdleGoingToActive {
        idle_since: DateTime<Utc>,
        transitioning_since: DateTime<Utc>,
    },
    ActiveGoingToIdle {
        active_since: DateTime<Utc>,
        transitioning_since: DateTime<Utc>,
    },
    Active {
        active_since: DateTime<Utc>,
    },
}

impl DebouncedIdleState {
    pub fn is_user_active(&self) -> bool {
        match self {
            DebouncedIdleState::Active { .. } => true,
            DebouncedIdleState::ActiveGoingToIdle { .. } => true,
            DebouncedIdleState::IdleGoingToActive { .. } => false,
            DebouncedIdleState::Idle { .. } => false,
        }
    }
}

impl fmt::Display for DebouncedIdleState {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

#[derive(Debug, PartialEq, Clone, Copy, Serialize, Deserialize)]
#[serde(tag = "state")]
pub enum ModeState {
    Normal {
        progress_towards_break: Duration,
        progress_towards_reset: Duration,
        idle_state: DebouncedIdleState,
    },
    PreBreak {
        started_at: DateTime<Utc>,
    },
    Break {
        progress_towards_finish: Duration,
        idle_state: DebouncedIdleState,
    },
}

#[derive(Debug, Eq, PartialEq, Clone, Copy, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", content = "value")]
pub enum PresenceMode {
    Active,
    SnoozedUntil(DateTime<Utc>),
    Muted,
}

#[derive(Debug, PartialEq, Clone, Copy, Serialize, Deserialize)]
pub struct IdleInfo {
    pub idle_since_seconds: u64,
    pub last_checked: DateTime<Utc>,
    pub last_mode_state: ModeState,
    pub reading_mode: bool,
    pub presence_mode: PresenceMode,
    pub time_to_break_secs: i64,
    pub break_length_secs: i64,
    pub overrun: Duration,
}

impl IdleInfo {
    pub fn is_muted(&self) -> bool {
        match self.presence_mode {
            PresenceMode::Active => false,
            PresenceMode::SnoozedUntil(_) => true,
            PresenceMode::Muted => true,
        }
    }
}

pub struct IdleMonitor<T: AbstractIdleChecker, U: AbstractClock> {
    idle_checker: T,
    clock: U,
    last_idle_info: IdleInfo,
}

impl<T: AbstractIdleChecker, U: AbstractClock> IdleMonitor<T, U> {
    pub fn new(idle_checker: T, clock: U, restored_state: Option<PersistableState>) -> Self {
        let time = clock.get_time();
        let use_restored_timers = match restored_state {
            Some(ref state) => {
                let time_since_last_check = time.signed_duration_since(state.last_checked);
                let sum_of_check_delta_and_reset_progress = time_since_last_check
                    .checked_add(&state.progress_towards_reset)
                    .unwrap();
                sum_of_check_delta_and_reset_progress < Duration::seconds(DEFAULT_BREAK_LENGTH_SECS)
            }
            None => false,
        };
        Self {
            idle_checker,
            clock,
            last_idle_info: IdleInfo {
                presence_mode: match restored_state {
                    Some(ref state) => state.presence_mode,
                    None => PresenceMode::Active,
                },
                idle_since_seconds: 0,
                last_checked: time,
                last_mode_state: ModeState::Normal {
                    idle_state: match use_restored_timers {
                        true => DebouncedIdleState::Idle {
                            idle_since: match restored_state {
                                Some(ref state) => state.last_checked,
                                None => time,
                            },
                        },
                        false => DebouncedIdleState::Active { active_since: time },
                    },
                    progress_towards_break: match restored_state {
                        Some(ref state) => {
                            if use_restored_timers {
                                state.progress_towards_break
                            } else {
                                Duration::seconds(0)
                            }
                        }
                        None => Duration::seconds(0),
                    },
                    progress_towards_reset: match restored_state {
                        Some(ref state) => {
                            if use_restored_timers {
                                let time_since_last_checked: Duration =
                                    time.signed_duration_since(state.last_checked);
                                let unconstrained_duration = state
                                    .progress_towards_reset
                                    .checked_add(&time_since_last_checked)
                                    .unwrap();
                                if unconstrained_duration
                                    > Duration::seconds(DEFAULT_BREAK_LENGTH_SECS)
                                {
                                    Duration::seconds(DEFAULT_BREAK_LENGTH_SECS)
                                } else {
                                    unconstrained_duration
                                }
                            } else {
                                Duration::seconds(0)
                            }
                        }
                        None => Duration::seconds(0),
                    },
                },
                reading_mode: match restored_state {
                    Some(ref state) => state.reading_mode,
                    None => false,
                },
                time_to_break_secs: match restored_state {
                    Some(ref state) => state.time_to_break_secs,
                    None => DEFAULT_TIME_TO_BREAK_SECS,
                },
                break_length_secs: match restored_state {
                    Some(ref state) => state.break_length_secs,
                    None => DEFAULT_BREAK_LENGTH_SECS,
                },
                overrun: Duration::seconds(0),
            },
        }
    }

    fn _make_debounced_idle_state(
        &self,
        idle_since_seconds: u64,
        last_idle_state: DebouncedIdleState,
        time_since_last_check: Duration,
        check_time: DateTime<Utc>,
        start_of_transition_period: DateTime<Utc>,
        in_break: bool,
    ) -> DebouncedIdleState {
        match last_idle_state {
            DebouncedIdleState::Active { active_since: _ }
            | DebouncedIdleState::ActiveGoingToIdle {
                active_since: _,
                transitioning_since: _,
            } if time_since_last_check > Duration::seconds(FRAME_DROP_CUTOFF_POINT_SECS) => {
                DebouncedIdleState::Idle {
                    idle_since: check_time,
                }
            }
            DebouncedIdleState::Active { active_since } => match idle_since_seconds {
                0 => DebouncedIdleState::Active { active_since },
                _ => match self.last_idle_info.reading_mode && !in_break {
                    true => DebouncedIdleState::Active { active_since },
                    false => DebouncedIdleState::ActiveGoingToIdle {
                        active_since,
                        transitioning_since: check_time,
                    },
                },
            },
            DebouncedIdleState::ActiveGoingToIdle {
                active_since,
                transitioning_since,
            } => match idle_since_seconds {
                0 => DebouncedIdleState::Active { active_since },
                1..=TRANSITION_THRESHOLD_SECS => DebouncedIdleState::ActiveGoingToIdle {
                    active_since,
                    transitioning_since,
                },
                _ => DebouncedIdleState::Idle {
                    idle_since: check_time,
                },
            },
            DebouncedIdleState::IdleGoingToActive {
                idle_since,
                transitioning_since,
            } => match idle_since_seconds {
                0..=END_OF_ACTIVE_PUSHING_TRANSITION_WINDOW
                    if transitioning_since <= start_of_transition_period =>
                {
                    DebouncedIdleState::Active {
                        active_since: check_time,
                    }
                }
                _ if transitioning_since <= start_of_transition_period => {
                    DebouncedIdleState::Idle { idle_since }
                }
                _ => DebouncedIdleState::IdleGoingToActive {
                    idle_since,
                    transitioning_since,
                },
            },
            DebouncedIdleState::Idle { idle_since } => match idle_since_seconds {
                0 => DebouncedIdleState::IdleGoingToActive {
                    idle_since,
                    transitioning_since: check_time,
                },
                _ => DebouncedIdleState::Idle { idle_since },
            },
        }
    }

    fn _make_idle_info_in_normal_state(
        &mut self,
        presence_mode: PresenceMode,
        idle_since_seconds: u64,
        progress_towards_break: Duration,
        progress_towards_reset: Duration,
        prv_idle_state: DebouncedIdleState,
        check_time: DateTime<Utc>,
        new_idle_state: DebouncedIdleState,
        time_since_last_check: Duration,
        reading_mode: bool,
        time_to_break_secs: i64,
        break_length_secs: i64,
        overrun: Duration,
    ) -> IdleInfo {
        IdleInfo {
            presence_mode,
            idle_since_seconds,
            last_checked: check_time,
            last_mode_state: ModeState::Normal {
                idle_state: new_idle_state,
                progress_towards_break: match prv_idle_state {
                    DebouncedIdleState::Active { active_since: _ }
                    | DebouncedIdleState::ActiveGoingToIdle {
                        active_since: _,
                        transitioning_since: _,
                    } if time_since_last_check
                        > Duration::seconds(FRAME_DROP_CUTOFF_POINT_SECS) =>
                    {
                        progress_towards_break
                    }
                    DebouncedIdleState::Idle { idle_since: _ }
                    | DebouncedIdleState::IdleGoingToActive {
                        idle_since: _,
                        transitioning_since: _,
                    } if progress_towards_reset + time_since_last_check
                        >= Duration::seconds(self.last_idle_info.break_length_secs) =>
                    {
                        match new_idle_state {
                            DebouncedIdleState::Idle { idle_since: _ }
                            | DebouncedIdleState::IdleGoingToActive {
                                idle_since: _,
                                transitioning_since: _,
                            } => Duration::seconds(0),
                            _ => progress_towards_break,
                        }
                    }
                    _ => Duration::seconds(self.last_idle_info.time_to_break_secs)
                        .min(progress_towards_break + time_since_last_check),
                },
                progress_towards_reset: match new_idle_state {
                    DebouncedIdleState::Idle { idle_since } if idle_since == check_time => {
                        Duration::seconds(0)
                    }
                    DebouncedIdleState::Active { active_since: _ }
                    | DebouncedIdleState::ActiveGoingToIdle {
                        active_since: _,
                        transitioning_since: _,
                    } => Duration::seconds(0),
                    _ => Duration::seconds(self.last_idle_info.break_length_secs)
                        .min(progress_towards_reset + time_since_last_check),
                },
            },
            reading_mode,
            break_length_secs,
            time_to_break_secs,
            overrun,
        }
    }

    fn _make_idle_info_in_prebreak_state(
        &mut self,
        idle_since_seconds: u64,
        last_checked: DateTime<Utc>,
        waiting_since: DateTime<Utc>,
        presence_mode: PresenceMode,
        reading_mode: bool,
        break_length_secs: i64,
        time_to_break_secs: i64,
        overrun: Duration,
    ) -> IdleInfo {
        IdleInfo {
            idle_since_seconds,
            last_checked,
            last_mode_state: ModeState::PreBreak {
                started_at: waiting_since,
            },
            presence_mode,
            reading_mode,
            break_length_secs,
            time_to_break_secs,
            overrun,
        }
    }

    fn _make_idle_info_in_break_state(
        &mut self,
        idle_since_seconds: u64,
        last_checked: DateTime<Utc>,
        new_idle_state: DebouncedIdleState,
        progress_towards_finish: Duration,
        time_since_last_check: Duration,
        presence_mode: PresenceMode,
        reading_mode: bool,
        time_to_break_secs: i64,
        break_length_secs: i64,
        overrun: Duration,
    ) -> IdleInfo {
        IdleInfo {
            idle_since_seconds,
            last_checked,
            last_mode_state: ModeState::Break {
                progress_towards_finish: match new_idle_state {
                    DebouncedIdleState::Idle { idle_since: _ }
                    | DebouncedIdleState::IdleGoingToActive {
                        idle_since: _,
                        transitioning_since: _,
                    } => Duration::seconds(break_length_secs)
                        .min(progress_towards_finish + time_since_last_check),
                    _ => progress_towards_finish,
                },
                idle_state: new_idle_state,
            },
            presence_mode,
            reading_mode,
            time_to_break_secs,
            break_length_secs,
            overrun,
        }
    }

    pub fn snooze(&mut self, timestamp: DateTime<Utc>) -> IdleInfo {
        self.last_idle_info.presence_mode = PresenceMode::SnoozedUntil(timestamp);
        self.persist_settings_to_disk();
        self.last_idle_info.clone()
    }

    fn persist_settings_to_disk(&self) {
        if let Err(_) = self.export_persistable_state().save_to_disk() {
            println!("Could not save settings and timer state to disk");
        }
    }

    pub fn mute(&mut self) -> IdleInfo {
        self.last_idle_info.presence_mode = PresenceMode::Muted;
        self.persist_settings_to_disk();
        self.last_idle_info.clone()
    }

    pub fn unmute(&mut self) -> IdleInfo {
        self.last_idle_info.presence_mode = PresenceMode::Active;
        self.persist_settings_to_disk();
        self.last_idle_info.clone()
    }

    pub fn set_reading_mode(&mut self, reading_mode: bool) {
        let check_time = self.clock.get_time();

        fn map_debounced_idle_state(
            debounced_idle_state: DebouncedIdleState,
            check_time: DateTime<Utc>,
        ) -> DebouncedIdleState {
            match debounced_idle_state {
                DebouncedIdleState::Active { active_since } => {
                    DebouncedIdleState::Active { active_since }
                }
                DebouncedIdleState::ActiveGoingToIdle { active_since, .. } => {
                    DebouncedIdleState::Active { active_since }
                }
                DebouncedIdleState::IdleGoingToActive { .. } | DebouncedIdleState::Idle { .. } => {
                    DebouncedIdleState::Active {
                        active_since: check_time,
                    }
                }
            }
        }

        self.last_idle_info = IdleInfo {
            idle_since_seconds: self.last_idle_info.idle_since_seconds,
            last_checked: self.last_idle_info.last_checked,
            last_mode_state: match self.last_idle_info.last_mode_state {
                ModeState::Normal {
                    progress_towards_break,
                    progress_towards_reset,
                    idle_state,
                } => ModeState::Normal {
                    progress_towards_break,
                    progress_towards_reset,
                    idle_state: map_debounced_idle_state(idle_state, check_time),
                },
                ModeState::PreBreak { started_at } => ModeState::PreBreak { started_at },
                ModeState::Break {
                    progress_towards_finish,
                    idle_state,
                } => ModeState::Break {
                    progress_towards_finish,
                    idle_state: map_debounced_idle_state(idle_state, check_time),
                },
            },
            reading_mode: reading_mode,
            presence_mode: self.last_idle_info.presence_mode,
            time_to_break_secs: self.last_idle_info.time_to_break_secs,
            break_length_secs: self.last_idle_info.break_length_secs,
            overrun: self.last_idle_info.overrun,
        };

        self.persist_settings_to_disk();
    }

    pub fn get_last_idle_info(&self) -> IdleInfo {
        self.last_idle_info.clone()
    }

    pub fn refresh_idle_info(&mut self) -> IdleInfo {
        let idle_since_seconds = self.idle_checker.get_idle_time_in_seconds();
        let check_time = self.clock.get_time();
        let start_of_transition_period = self.clock.get_time()
            - Duration::seconds(TRANSITION_THRESHOLD_SECS.try_into().unwrap());
        let time_since_last_check =
            check_time.signed_duration_since(self.last_idle_info.last_checked);
        let last_mode_state = self.last_idle_info.last_mode_state.clone();

        let new_presence_mode = match self.last_idle_info.presence_mode {
            PresenceMode::Active => PresenceMode::Active,
            PresenceMode::SnoozedUntil(timestamp) if timestamp < check_time => PresenceMode::Active,
            PresenceMode::SnoozedUntil(timestamp) => PresenceMode::SnoozedUntil(timestamp),
            PresenceMode::Muted => PresenceMode::Muted,
        };

        self.last_idle_info = match last_mode_state {
            ModeState::Normal {
                progress_towards_break,
                progress_towards_reset: _,
                idle_state: _,
            } if progress_towards_break + time_since_last_check
                >= Duration::seconds(self.last_idle_info.time_to_break_secs)
                && time_since_last_check < Duration::seconds(FRAME_DROP_CUTOFF_POINT_SECS)
                && !self.last_idle_info.is_muted() =>
            {
                self._make_idle_info_in_prebreak_state(
                    idle_since_seconds,
                    check_time,
                    self.clock.get_time(),
                    new_presence_mode,
                    self.last_idle_info.reading_mode,
                    self.last_idle_info.break_length_secs,
                    self.last_idle_info.time_to_break_secs,
                    self.last_idle_info.overrun,
                )
            }
            ModeState::Normal {
                progress_towards_break,
                progress_towards_reset,
                idle_state,
            } => self._make_idle_info_in_normal_state(
                new_presence_mode,
                idle_since_seconds,
                if time_since_last_check > Duration::seconds(self.last_idle_info.time_to_break_secs)
                {
                    Duration::seconds(0)
                } else {
                    progress_towards_break
                },
                progress_towards_reset,
                idle_state,
                check_time,
                self._make_debounced_idle_state(
                    idle_since_seconds,
                    idle_state,
                    time_since_last_check,
                    check_time,
                    start_of_transition_period,
                    false,
                ),
                time_since_last_check,
                self.last_idle_info.reading_mode,
                self.last_idle_info.time_to_break_secs,
                self.last_idle_info.break_length_secs,
                if (progress_towards_reset + time_since_last_check).num_seconds()
                    >= self.last_idle_info.break_length_secs
                {
                    Duration::seconds(0)
                } else {
                    match idle_state {
                        DebouncedIdleState::Active { .. }
                        | DebouncedIdleState::ActiveGoingToIdle { .. } => {
                            if self.last_idle_info.overrun == Duration::seconds(0)
                                && progress_towards_break.num_seconds()
                                    < self.last_idle_info.time_to_break_secs
                            {
                                self.last_idle_info.overrun
                            } else {
                                self.last_idle_info.overrun
                                    + (check_time - self.last_idle_info.last_checked)
                            }
                        }
                        DebouncedIdleState::Idle { .. }
                        | DebouncedIdleState::IdleGoingToActive { .. } => {
                            self.last_idle_info.overrun
                        }
                    }
                },
            ),
            ModeState::PreBreak { .. } if self.last_idle_info.is_muted() => self
                ._make_idle_info_in_normal_state(
                    new_presence_mode,
                    idle_since_seconds,
                    Duration::seconds(self.last_idle_info.time_to_break_secs),
                    Duration::seconds(0),
                    DebouncedIdleState::Active {
                        active_since: check_time,
                    },
                    check_time,
                    self._make_debounced_idle_state(
                        idle_since_seconds,
                        DebouncedIdleState::Active {
                            active_since: check_time,
                        },
                        time_since_last_check,
                        check_time,
                        start_of_transition_period,
                        false,
                    ),
                    time_since_last_check,
                    self.last_idle_info.reading_mode,
                    self.last_idle_info.time_to_break_secs,
                    self.last_idle_info.break_length_secs,
                    self.last_idle_info.overrun,
                ),
            ModeState::PreBreak { .. }
                if idle_since_seconds >= REQUIRED_PREBREAK_IDLE_STREAK_SECONDS =>
            {
                IdleInfo {
                    idle_since_seconds,
                    last_checked: check_time,
                    last_mode_state: ModeState::Break {
                        progress_towards_finish: Duration::seconds(0),
                        idle_state: DebouncedIdleState::Idle {
                            idle_since: check_time
                                - Duration::seconds(
                                    idle_since_seconds.try_into().expect("Integer cast failed"),
                                ),
                        },
                    },
                    presence_mode: new_presence_mode,
                    reading_mode: self.last_idle_info.reading_mode,
                    break_length_secs: self.last_idle_info.break_length_secs,
                    time_to_break_secs: self.last_idle_info.time_to_break_secs,
                    overrun: self.last_idle_info.overrun
                        + (check_time - self.last_idle_info.last_checked),
                }
            }
            ModeState::PreBreak {
                started_at: waiting_since,
            } => self._make_idle_info_in_prebreak_state(
                idle_since_seconds,
                check_time,
                waiting_since,
                new_presence_mode,
                self.last_idle_info.reading_mode,
                self.last_idle_info.break_length_secs,
                self.last_idle_info.time_to_break_secs,
                self.last_idle_info.overrun + (check_time - self.last_idle_info.last_checked),
            ),
            ModeState::Break {
                progress_towards_finish,
                idle_state,
            } if progress_towards_finish + time_since_last_check
                >= Duration::seconds(self.last_idle_info.break_length_secs) =>
            {
                IdleInfo {
                    idle_since_seconds,
                    last_checked: check_time,
                    last_mode_state: ModeState::Normal {
                        progress_towards_break: Duration::seconds(0),
                        progress_towards_reset: Duration::seconds(
                            self.last_idle_info.break_length_secs,
                        ),
                        idle_state: self._make_debounced_idle_state(
                            idle_since_seconds,
                            idle_state,
                            time_since_last_check,
                            check_time,
                            start_of_transition_period,
                            true,
                        ),
                    },
                    presence_mode: new_presence_mode,
                    reading_mode: self.last_idle_info.reading_mode,
                    time_to_break_secs: self.last_idle_info.time_to_break_secs,
                    break_length_secs: self.last_idle_info.break_length_secs,
                    overrun: Duration::seconds(0),
                }
            }
            ModeState::Break {
                progress_towards_finish,
                idle_state,
            } => self._make_idle_info_in_break_state(
                idle_since_seconds,
                check_time,
                self._make_debounced_idle_state(
                    idle_since_seconds,
                    idle_state,
                    time_since_last_check,
                    check_time,
                    start_of_transition_period,
                    true,
                ),
                progress_towards_finish,
                time_since_last_check,
                new_presence_mode,
                self.last_idle_info.reading_mode,
                self.last_idle_info.time_to_break_secs,
                self.last_idle_info.break_length_secs,
                match idle_state {
                    DebouncedIdleState::Active { .. }
                    | DebouncedIdleState::ActiveGoingToIdle { .. } => {
                        self.last_idle_info.overrun
                            + (check_time - self.last_idle_info.last_checked)
                    }
                    DebouncedIdleState::Idle { .. }
                    | DebouncedIdleState::IdleGoingToActive { .. } => self.last_idle_info.overrun,
                },
            ),
        };

        return self.last_idle_info.clone();
    }

    pub fn trigger_break(&mut self) -> IdleInfo {
        self.last_idle_info = match self.last_idle_info.last_mode_state {
            ModeState::Normal {
                progress_towards_break: _,
                progress_towards_reset: _,
                idle_state: _,
            } => IdleInfo {
                idle_since_seconds: self.last_idle_info.idle_since_seconds,
                last_checked: self.last_idle_info.last_checked,
                last_mode_state: ModeState::Break {
                    progress_towards_finish: Duration::seconds(0),
                    idle_state: DebouncedIdleState::Idle {
                        idle_since: self.last_idle_info.last_checked,
                    },
                },
                presence_mode: self.last_idle_info.presence_mode,
                reading_mode: self.last_idle_info.reading_mode,
                time_to_break_secs: self.last_idle_info.time_to_break_secs,
                break_length_secs: self.last_idle_info.break_length_secs,
                overrun: self.last_idle_info.overrun,
            },
            _ => self.last_idle_info,
        };

        return self.last_idle_info.clone();
    }

    pub fn skip_break(&mut self) -> IdleInfo {
        let check_time = self.clock.get_time();
        let idle_since_seconds = self.idle_checker.get_idle_time_in_seconds();

        self.last_idle_info = match self.last_idle_info.last_mode_state {
            ModeState::Break {
                progress_towards_finish: _,
                idle_state,
            } => IdleInfo {
                idle_since_seconds,
                last_checked: check_time,
                last_mode_state: ModeState::Normal {
                    progress_towards_break: Duration::seconds(0),
                    progress_towards_reset: Duration::seconds(0),
                    idle_state,
                },
                presence_mode: self.last_idle_info.presence_mode,
                reading_mode: self.last_idle_info.reading_mode,
                time_to_break_secs: self.last_idle_info.time_to_break_secs,
                break_length_secs: self.last_idle_info.break_length_secs,
                overrun: Duration::seconds(0),
            },
            _ => self.last_idle_info,
        };

        return self.last_idle_info.clone();
    }

    pub fn postpone_break(&mut self, postpone_duration: Duration) -> IdleInfo {
        let check_time = self.clock.get_time();
        let idle_since_seconds = self.idle_checker.get_idle_time_in_seconds();

        self.last_idle_info = match self.last_idle_info.last_mode_state {
            ModeState::Break {
                progress_towards_finish: _,
                idle_state,
            } => IdleInfo {
                idle_since_seconds,
                last_checked: check_time,
                last_mode_state: ModeState::Normal {
                    progress_towards_break: Duration::seconds(
                        self.last_idle_info.time_to_break_secs,
                    ) - postpone_duration,
                    progress_towards_reset: Duration::seconds(0),
                    idle_state,
                },
                presence_mode: self.last_idle_info.presence_mode,
                reading_mode: self.last_idle_info.reading_mode,
                time_to_break_secs: self.last_idle_info.time_to_break_secs,
                break_length_secs: self.last_idle_info.break_length_secs,
                overrun: self.last_idle_info.overrun,
            },
            _ => self.last_idle_info,
        };
        return self.last_idle_info.clone();
    }

    pub fn set_time_to_break(&mut self, num_secs: i64) {
        self.last_idle_info.time_to_break_secs = num_secs;
        self.last_idle_info.last_mode_state = match self.last_idle_info.last_mode_state {
            ModeState::Normal {
                progress_towards_break,
                progress_towards_reset,
                idle_state,
            } => ModeState::Normal {
                progress_towards_break: Duration::seconds(min(
                    progress_towards_break.num_seconds(),
                    num_secs,
                )),
                progress_towards_reset,
                idle_state,
            },
            ModeState::PreBreak { started_at } => ModeState::PreBreak { started_at },
            ModeState::Break {
                progress_towards_finish,
                idle_state,
            } => ModeState::Break {
                progress_towards_finish,
                idle_state,
            },
        };
        self.persist_settings_to_disk();
    }

    pub fn set_break_length(&mut self, num_secs: i64) {
        self.last_idle_info.break_length_secs = num_secs;
        self.last_idle_info.last_mode_state = match self.last_idle_info.last_mode_state {
            ModeState::Normal {
                progress_towards_break,
                progress_towards_reset,
                idle_state,
            } => ModeState::Normal {
                progress_towards_break,
                progress_towards_reset: Duration::seconds(min(
                    progress_towards_reset.num_seconds(),
                    num_secs,
                )),
                idle_state,
            },
            ModeState::PreBreak { started_at } => ModeState::PreBreak { started_at },
            ModeState::Break {
                progress_towards_finish,
                idle_state,
            } => ModeState::Break {
                progress_towards_finish,
                idle_state,
            },
        };
        self.persist_settings_to_disk();
    }

    pub fn export_persistable_state(&self) -> PersistableState {
        PersistableState {
            progress_towards_break: match self.last_idle_info.last_mode_state {
                ModeState::Normal {
                    progress_towards_break,
                    ..
                } => progress_towards_break,
                _ => Duration::seconds(0),
            },
            progress_towards_reset: match self.last_idle_info.last_mode_state {
                ModeState::Normal {
                    progress_towards_reset,
                    ..
                } => progress_towards_reset,
                _ => Duration::seconds(0),
            },
            last_checked: self.last_idle_info.last_checked,
            presence_mode: self.last_idle_info.presence_mode,
            reading_mode: self.last_idle_info.reading_mode,
            time_to_break_secs: self.last_idle_info.time_to_break_secs,
            break_length_secs: self.last_idle_info.break_length_secs,
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};

    use super::*;

    fn make_idle_checker(idle_value: u64) -> MockIdleChecker {
        let mut idle_checker = MockIdleChecker::new();
        idle_checker
            .expect_get_idle_time_in_seconds()
            .return_once(move || idle_value);
        return idle_checker;
    }

    fn make_clock(frozen_value: &DateTime<Utc>) -> MockClock {
        let mut clock = MockClock::new();
        clock.expect_get_time().return_const(frozen_value.clone());
        return clock;
    }

    #[test]
    fn freshly_initialized() {
        let current_time = Utc::now();
        let idle_checker = make_idle_checker(0);
        let clock = make_clock(&current_time);

        let mut idle_monitor = IdleMonitor::new(idle_checker, clock, None);
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 0,
            last_checked: current_time,
            last_mode_state: ModeState::Normal {
                progress_towards_break: Duration::seconds(0),
                progress_towards_reset: Duration::seconds(0),
                idle_state: DebouncedIdleState::Active {
                    active_since: current_time,
                },
            },
            presence_mode: PresenceMode::Active,
            reading_mode: false,
            time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
            break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
            overrun: Duration::seconds(0),
        };
        assert_eq!(idle_monitor.refresh_idle_info(), expected_idle_info);
    }

    #[test]
    fn initialized_with_restored_state_timers_needing_clean_slate() {
        let current_time = Utc::now();
        let idle_checker = make_idle_checker(0);
        let clock = make_clock(&current_time);

        let mut idle_monitor = IdleMonitor::new(
            idle_checker,
            clock,
            Some(PersistableState {
                progress_towards_break: Duration::seconds(DEFAULT_TIME_TO_BREAK_SECS - 1),
                progress_towards_reset: Duration::seconds(5),
                last_checked: current_time - Duration::seconds(DEFAULT_BREAK_LENGTH_SECS - 5 + 1),
                presence_mode: PresenceMode::Muted,
                reading_mode: false,
                time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
                break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
            }),
        );
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 0,
            last_checked: current_time,
            last_mode_state: ModeState::Normal {
                progress_towards_break: Duration::seconds(0),
                progress_towards_reset: Duration::seconds(0),
                idle_state: DebouncedIdleState::Active {
                    active_since: current_time,
                },
            },
            presence_mode: PresenceMode::Muted,
            reading_mode: false,
            time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
            break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
            overrun: Duration::seconds(0),
        };
        assert_eq!(idle_monitor.refresh_idle_info(), expected_idle_info);
    }

    #[test]
    fn initialized_with_restored_state_timers_that_can_continue() {
        let current_time = Utc::now();
        let idle_checker = make_idle_checker(1);
        let clock = make_clock(&current_time);

        let mut idle_monitor = IdleMonitor::new(
            idle_checker,
            clock,
            Some(PersistableState {
                progress_towards_break: Duration::seconds(DEFAULT_TIME_TO_BREAK_SECS - 1),
                progress_towards_reset: Duration::seconds(5),
                last_checked: current_time - Duration::seconds(DEFAULT_BREAK_LENGTH_SECS - 5 - 1),
                presence_mode: PresenceMode::SnoozedUntil(current_time + Duration::minutes(30)),
                reading_mode: true,
                time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
                break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
            }),
        );
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 1,
            last_checked: current_time,
            last_mode_state: ModeState::Normal {
                progress_towards_break: Duration::seconds(DEFAULT_TIME_TO_BREAK_SECS - 1),
                progress_towards_reset: Duration::seconds(DEFAULT_BREAK_LENGTH_SECS - 1),
                idle_state: DebouncedIdleState::Idle {
                    idle_since: current_time - Duration::seconds(DEFAULT_BREAK_LENGTH_SECS - 5 - 1),
                },
            },
            presence_mode: PresenceMode::SnoozedUntil(current_time + Duration::minutes(30)),
            reading_mode: true,
            time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
            break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
            overrun: Duration::seconds(0),
        };
        assert_eq!(idle_monitor.refresh_idle_info(), expected_idle_info);
    }

    #[test]
    fn active_status_quo() {
        let current_time = Utc::now();
        let idle_checker = make_idle_checker(0);
        let clock = make_clock(&current_time);

        let mut idle_monitor = IdleMonitor {
            idle_checker,
            clock,
            last_idle_info: IdleInfo {
                idle_since_seconds: 0,
                last_checked: current_time - Duration::milliseconds(1009),
                last_mode_state: ModeState::Normal {
                    progress_towards_break: Duration::milliseconds(20001),
                    progress_towards_reset: Duration::seconds(0),
                    idle_state: DebouncedIdleState::Active {
                        active_since: current_time - Duration::milliseconds(10000),
                    },
                },
                presence_mode: PresenceMode::Active,
                reading_mode: false,
                time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
                break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
                overrun: Duration::seconds(0),
            },
        };
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 0,
            last_checked: current_time,
            last_mode_state: ModeState::Normal {
                progress_towards_break: Duration::milliseconds(21010),
                progress_towards_reset: Duration::seconds(0),
                idle_state: DebouncedIdleState::Active {
                    active_since: current_time - Duration::milliseconds(10000),
                },
            },
            presence_mode: PresenceMode::Active,
            reading_mode: false,
            time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
            break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
            overrun: Duration::seconds(0),
        };
        assert_eq!(idle_monitor.refresh_idle_info(), expected_idle_info);
    }

    #[test]
    fn active_increments_overrun_when_nonzero() {
        let current_time = Utc::now();
        let idle_checker = make_idle_checker(0);
        let clock = make_clock(&current_time);

        let mut idle_monitor = IdleMonitor {
            idle_checker,
            clock,
            last_idle_info: IdleInfo {
                idle_since_seconds: 0,
                last_checked: current_time - Duration::milliseconds(1_009),
                last_mode_state: ModeState::Normal {
                    progress_towards_break: Duration::milliseconds(20_001),
                    progress_towards_reset: Duration::seconds(0),
                    idle_state: DebouncedIdleState::Active {
                        active_since: current_time - Duration::milliseconds(10_000),
                    },
                },
                presence_mode: PresenceMode::Active,
                reading_mode: false,
                time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
                break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
                overrun: Duration::milliseconds(18_000),
            },
        };
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 0,
            last_checked: current_time,
            last_mode_state: ModeState::Normal {
                progress_towards_break: Duration::milliseconds(21010),
                progress_towards_reset: Duration::seconds(0),
                idle_state: DebouncedIdleState::Active {
                    active_since: current_time - Duration::milliseconds(10000),
                },
            },
            presence_mode: PresenceMode::Active,
            reading_mode: false,
            time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
            break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
            overrun: Duration::milliseconds(19_009),
        };
        assert_eq!(idle_monitor.refresh_idle_info(), expected_idle_info);
    }

    #[test]
    fn ignore_jumps_in_last_check_time_above_cutoff_point_below_time_to_break() {
        let current_time = Utc::now();
        let idle_checker = make_idle_checker(0);
        let clock = make_clock(&current_time);

        let mut idle_monitor = IdleMonitor {
            idle_checker,
            clock,
            last_idle_info: IdleInfo {
                idle_since_seconds: 0,
                last_checked: current_time - Duration::seconds(FRAME_DROP_CUTOFF_POINT_SECS + 1),
                last_mode_state: ModeState::Normal {
                    progress_towards_break: Duration::milliseconds(20_001),
                    progress_towards_reset: Duration::seconds(0),
                    idle_state: DebouncedIdleState::Active {
                        active_since: current_time - Duration::milliseconds(10_000),
                    },
                },
                presence_mode: PresenceMode::Active,
                reading_mode: false,
                time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
                break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
                overrun: Duration::seconds(0),
            },
        };
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 0,
            last_checked: current_time,
            last_mode_state: ModeState::Normal {
                progress_towards_break: Duration::milliseconds(20_001),
                progress_towards_reset: Duration::seconds(0),
                idle_state: DebouncedIdleState::Idle {
                    idle_since: current_time,
                },
            },
            presence_mode: PresenceMode::Active,
            reading_mode: false,
            time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
            break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
            overrun: Duration::seconds(0),
        };
        assert_eq!(idle_monitor.refresh_idle_info(), expected_idle_info);
    }

    #[test]
    fn ignore_jumps_in_last_check_time_above_time_to_break() {
        // This is to handle standby
        let current_time = Utc::now();
        let idle_checker = make_idle_checker(4643);
        let clock = make_clock(&current_time);

        let mut idle_monitor = IdleMonitor {
            idle_checker,
            clock,
            last_idle_info: IdleInfo {
                idle_since_seconds: 0,
                last_checked: current_time - Duration::seconds(DEFAULT_TIME_TO_BREAK_SECS + 1),
                last_mode_state: ModeState::Normal {
                    progress_towards_break: Duration::seconds(14),
                    progress_towards_reset: Duration::seconds(0),
                    idle_state: DebouncedIdleState::Active {
                        active_since: current_time - Duration::seconds(15),
                    },
                },
                presence_mode: PresenceMode::Active,
                reading_mode: false,
                time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
                break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
                overrun: Duration::seconds(0),
            },
        };
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 4643,
            last_checked: current_time,
            last_mode_state: ModeState::Normal {
                progress_towards_break: Duration::seconds(0),
                progress_towards_reset: Duration::seconds(0),
                idle_state: DebouncedIdleState::Idle {
                    idle_since: current_time,
                },
            },
            presence_mode: PresenceMode::Active,
            reading_mode: false,
            time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
            break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
            overrun: Duration::seconds(0),
        };
        assert_eq!(idle_monitor.refresh_idle_info(), expected_idle_info);
    }

    #[test]
    fn idle_status_quo() {
        let current_time = Utc::now();
        let idle_checker = make_idle_checker(1);
        let clock = make_clock(&current_time);

        let mut idle_monitor = IdleMonitor {
            idle_checker,
            clock,
            last_idle_info: IdleInfo {
                idle_since_seconds: 0,
                last_checked: current_time - Duration::milliseconds(1025),
                last_mode_state: ModeState::Normal {
                    progress_towards_break: Duration::milliseconds(6000),
                    progress_towards_reset: Duration::milliseconds(2000),
                    idle_state: DebouncedIdleState::Idle {
                        idle_since: current_time - Duration::milliseconds(5000),
                    },
                },
                presence_mode: PresenceMode::Active,
                reading_mode: false,
                time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
                break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
                overrun: Duration::seconds(0),
            },
        };
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 1,
            last_checked: current_time,
            last_mode_state: ModeState::Normal {
                progress_towards_break: Duration::milliseconds(7025),
                progress_towards_reset: Duration::milliseconds(3025),
                idle_state: DebouncedIdleState::Idle {
                    idle_since: current_time - Duration::milliseconds(5000),
                },
            },
            presence_mode: PresenceMode::Active,
            reading_mode: false,
            time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
            break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
            overrun: Duration::seconds(0),
        };
        assert_eq!(idle_monitor.refresh_idle_info(), expected_idle_info);
    }

    #[test]
    fn idle_starting_transition() {
        let current_time = Utc::now();
        let idle_checker = make_idle_checker(0);
        let clock = make_clock(&current_time);

        let mut idle_monitor = IdleMonitor {
            idle_checker,
            clock,
            last_idle_info: IdleInfo {
                idle_since_seconds: 5,
                last_checked: current_time - Duration::milliseconds(1999),
                last_mode_state: ModeState::Normal {
                    progress_towards_break: Duration::milliseconds(24_000),
                    progress_towards_reset: Duration::milliseconds(2_000),
                    idle_state: DebouncedIdleState::Idle {
                        idle_since: current_time - Duration::milliseconds(5_000),
                    },
                },
                presence_mode: PresenceMode::Active,
                reading_mode: false,
                time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
                break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
                overrun: Duration::seconds(0),
            },
        };
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 0,
            last_checked: current_time,
            last_mode_state: ModeState::Normal {
                progress_towards_break: Duration::milliseconds(25_999),
                progress_towards_reset: Duration::milliseconds(3_999),
                idle_state: DebouncedIdleState::IdleGoingToActive {
                    idle_since: current_time - Duration::milliseconds(5_000),
                    transitioning_since: current_time,
                },
            },
            presence_mode: PresenceMode::Active,
            reading_mode: false,
            time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
            break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
            overrun: Duration::seconds(0),
        };
        assert_eq!(idle_monitor.refresh_idle_info(), expected_idle_info);
    }

    #[test]
    fn idle_still_transitioning() {
        let current_time = Utc::now();
        let idle_checker = make_idle_checker(1);
        let clock = make_clock(&current_time);

        let mut idle_monitor = IdleMonitor {
            idle_checker,
            clock,
            last_idle_info: IdleInfo {
                idle_since_seconds: 0,
                last_checked: current_time - Duration::milliseconds(1_999),
                last_mode_state: ModeState::Normal {
                    progress_towards_break: Duration::milliseconds(12_000),
                    progress_towards_reset: Duration::milliseconds(4_000),
                    idle_state: DebouncedIdleState::IdleGoingToActive {
                        idle_since: current_time - Duration::milliseconds(6_000),
                        transitioning_since: current_time - Duration::milliseconds(1_000),
                    },
                },
                presence_mode: PresenceMode::Active,
                reading_mode: false,
                time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
                break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
                overrun: Duration::seconds(0),
            },
        };
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 1,
            last_checked: current_time,
            last_mode_state: ModeState::Normal {
                progress_towards_break: Duration::milliseconds(13_999),
                progress_towards_reset: Duration::milliseconds(5_999),
                idle_state: DebouncedIdleState::IdleGoingToActive {
                    idle_since: current_time - Duration::milliseconds(6_000),
                    transitioning_since: current_time - Duration::milliseconds(1_000),
                },
            },
            presence_mode: PresenceMode::Active,
            reading_mode: false,
            time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
            break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
            overrun: Duration::seconds(0),
        };
        assert_eq!(idle_monitor.refresh_idle_info(), expected_idle_info);
    }

    #[test]
    fn idle_cancelling_transition() {
        let current_time = Utc::now();
        let idle_checker = make_idle_checker(3);
        let clock = make_clock(&current_time);

        let mut idle_monitor = IdleMonitor {
            idle_checker,
            clock,
            last_idle_info: IdleInfo {
                idle_since_seconds: 2,
                last_checked: current_time - Duration::milliseconds(1_000),
                last_mode_state: ModeState::Normal {
                    progress_towards_break: Duration::milliseconds(8_000),
                    progress_towards_reset: Duration::milliseconds(2_012),
                    idle_state: DebouncedIdleState::IdleGoingToActive {
                        idle_since: current_time - Duration::milliseconds(8_000),
                        transitioning_since: current_time - Duration::milliseconds(3_000),
                    },
                },
                presence_mode: PresenceMode::Active,
                reading_mode: false,
                time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
                break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
                overrun: Duration::seconds(0),
            },
        };
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 3,
            last_checked: current_time,
            last_mode_state: ModeState::Normal {
                progress_towards_break: Duration::milliseconds(9_000),
                progress_towards_reset: Duration::milliseconds(3_012),
                idle_state: DebouncedIdleState::Idle {
                    idle_since: current_time - Duration::milliseconds(8_000),
                },
            },
            presence_mode: PresenceMode::Active,
            reading_mode: false,
            time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
            break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
            overrun: Duration::seconds(0),
        };
        assert_eq!(idle_monitor.refresh_idle_info(), expected_idle_info);
    }

    #[test]
    fn idle_finalizing_transition() {
        let current_time = Utc::now();
        let idle_checker = make_idle_checker(1);
        let clock = make_clock(&current_time);

        let mut idle_monitor = IdleMonitor {
            idle_checker,
            clock,
            last_idle_info: IdleInfo {
                idle_since_seconds: 1,
                last_checked: current_time - Duration::milliseconds(1_123),
                last_mode_state: ModeState::Normal {
                    progress_towards_break: Duration::milliseconds(11_000),
                    progress_towards_reset: Duration::milliseconds(8_000),
                    idle_state: DebouncedIdleState::IdleGoingToActive {
                        idle_since: current_time - Duration::milliseconds(9_000),
                        transitioning_since: current_time - Duration::milliseconds(3_000),
                    },
                },
                presence_mode: PresenceMode::Active,
                reading_mode: false,
                time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
                break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
                overrun: Duration::seconds(0),
            },
        };
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 1,
            last_checked: current_time,
            last_mode_state: ModeState::Normal {
                progress_towards_break: Duration::milliseconds(12_123),
                progress_towards_reset: Duration::seconds(0),
                idle_state: DebouncedIdleState::Active {
                    active_since: current_time,
                },
            },
            presence_mode: PresenceMode::Active,
            reading_mode: false,
            time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
            break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
            overrun: Duration::seconds(0),
        };
        assert_eq!(idle_monitor.refresh_idle_info(), expected_idle_info);
    }

    #[test]
    fn active_starting_transition() {
        let current_time = Utc::now();
        let idle_checker = make_idle_checker(1);
        let clock = make_clock(&current_time);

        let mut idle_monitor = IdleMonitor {
            idle_checker,
            clock,
            last_idle_info: IdleInfo {
                idle_since_seconds: 0,
                last_checked: current_time - Duration::milliseconds(1_000),
                last_mode_state: ModeState::Normal {
                    progress_towards_break: Duration::milliseconds(8_000),
                    progress_towards_reset: Duration::seconds(0),
                    idle_state: DebouncedIdleState::Active {
                        active_since: current_time - Duration::milliseconds(1_000),
                    },
                },
                presence_mode: PresenceMode::Active,
                reading_mode: false,
                time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
                break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
                overrun: Duration::seconds(0),
            },
        };
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 1,
            last_checked: current_time,
            last_mode_state: ModeState::Normal {
                progress_towards_break: Duration::milliseconds(9_000),
                progress_towards_reset: Duration::seconds(0),
                idle_state: DebouncedIdleState::ActiveGoingToIdle {
                    active_since: current_time - Duration::milliseconds(1_000),
                    transitioning_since: current_time,
                },
            },
            presence_mode: PresenceMode::Active,
            reading_mode: false,
            time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
            break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
            overrun: Duration::seconds(0),
        };
        assert_eq!(idle_monitor.refresh_idle_info(), expected_idle_info);
    }

    #[test]
    fn active_dont_start_transition_to_idle_in_reading_mode() {
        let current_time = Utc::now();
        let idle_checker = make_idle_checker(1);
        let clock = make_clock(&current_time);

        let mut idle_monitor = IdleMonitor {
            idle_checker,
            clock,
            last_idle_info: IdleInfo {
                idle_since_seconds: 0,
                last_checked: current_time - Duration::milliseconds(1_000),
                last_mode_state: ModeState::Normal {
                    progress_towards_break: Duration::milliseconds(8_000),
                    progress_towards_reset: Duration::seconds(0),
                    idle_state: DebouncedIdleState::Active {
                        active_since: current_time - Duration::milliseconds(1_000),
                    },
                },
                presence_mode: PresenceMode::Active,
                reading_mode: true,
                time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
                break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
                overrun: Duration::seconds(0),
            },
        };
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 1,
            last_checked: current_time,
            last_mode_state: ModeState::Normal {
                progress_towards_break: Duration::milliseconds(9_000),
                progress_towards_reset: Duration::seconds(0),
                idle_state: DebouncedIdleState::Active {
                    active_since: current_time - Duration::milliseconds(1_000),
                },
            },
            presence_mode: PresenceMode::Active,
            reading_mode: true,
            time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
            break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
            overrun: Duration::seconds(0),
        };
        assert_eq!(idle_monitor.refresh_idle_info(), expected_idle_info);
    }

    #[test]
    fn active_still_transitioning() {
        let current_time = Utc::now();
        let idle_checker = make_idle_checker(2);
        let clock = make_clock(&current_time);

        let mut idle_monitor = IdleMonitor {
            idle_checker,
            clock,
            last_idle_info: IdleInfo {
                idle_since_seconds: 1,
                last_checked: current_time - Duration::milliseconds(1_001),
                last_mode_state: ModeState::Normal {
                    progress_towards_break: Duration::milliseconds(11_000),
                    progress_towards_reset: Duration::seconds(0),
                    idle_state: DebouncedIdleState::ActiveGoingToIdle {
                        active_since: current_time - Duration::milliseconds(8_000),
                        transitioning_since: current_time - Duration::milliseconds(1_000),
                    },
                },
                presence_mode: PresenceMode::Active,
                reading_mode: false,
                time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
                break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
                overrun: Duration::seconds(0),
            },
        };
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 2,
            last_checked: current_time,
            last_mode_state: ModeState::Normal {
                progress_towards_break: Duration::milliseconds(12_001),
                progress_towards_reset: Duration::seconds(0),
                idle_state: DebouncedIdleState::ActiveGoingToIdle {
                    active_since: current_time - Duration::milliseconds(8_000),
                    transitioning_since: current_time - Duration::milliseconds(1_000),
                },
            },
            presence_mode: PresenceMode::Active,
            reading_mode: false,
            time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
            break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
            overrun: Duration::seconds(0),
        };
        assert_eq!(idle_monitor.refresh_idle_info(), expected_idle_info);
    }

    #[test]
    fn active_cancelling_transition() {
        let current_time = Utc::now();
        let idle_checker = make_idle_checker(0);
        let clock = make_clock(&current_time);

        let mut idle_monitor = IdleMonitor {
            idle_checker,
            clock,
            last_idle_info: IdleInfo {
                idle_since_seconds: 3,
                last_checked: current_time - Duration::milliseconds(1_002),
                last_mode_state: ModeState::Normal {
                    progress_towards_break: Duration::milliseconds(11_005),
                    progress_towards_reset: Duration::seconds(0),
                    idle_state: DebouncedIdleState::ActiveGoingToIdle {
                        active_since: current_time - Duration::milliseconds(9_000),
                        transitioning_since: current_time - Duration::milliseconds(2_000),
                    },
                },
                presence_mode: PresenceMode::Active,
                reading_mode: false,
                time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
                break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
                overrun: Duration::seconds(0),
            },
        };
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 0,
            last_checked: current_time,
            last_mode_state: ModeState::Normal {
                progress_towards_break: Duration::milliseconds(12_007),
                progress_towards_reset: Duration::seconds(0),
                idle_state: DebouncedIdleState::Active {
                    active_since: current_time - Duration::milliseconds(9_000),
                },
            },
            presence_mode: PresenceMode::Active,
            reading_mode: false,
            time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
            break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
            overrun: Duration::seconds(0),
        };
        assert_eq!(idle_monitor.refresh_idle_info(), expected_idle_info);
    }

    #[test]
    fn active_finalizing_transition() {
        let current_time = Utc::now();
        let idle_checker = make_idle_checker(4);
        let clock = make_clock(&current_time);

        let mut idle_monitor = IdleMonitor {
            idle_checker,
            clock,
            last_idle_info: IdleInfo {
                idle_since_seconds: 3,
                last_checked: current_time - Duration::milliseconds(1_005),
                last_mode_state: ModeState::Normal {
                    progress_towards_break: Duration::milliseconds(14_020),
                    progress_towards_reset: Duration::seconds(0),
                    idle_state: DebouncedIdleState::ActiveGoingToIdle {
                        active_since: current_time - Duration::milliseconds(9_000),
                        transitioning_since: current_time - Duration::milliseconds(2_000),
                    },
                },
                presence_mode: PresenceMode::Active,
                reading_mode: false,
                time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
                break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
                overrun: Duration::seconds(0),
            },
        };
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 4,
            last_checked: current_time,
            last_mode_state: ModeState::Normal {
                progress_towards_break: Duration::milliseconds(15_025),
                progress_towards_reset: Duration::seconds(0),
                idle_state: DebouncedIdleState::Idle {
                    idle_since: current_time,
                },
            },
            presence_mode: PresenceMode::Active,
            reading_mode: false,
            time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
            break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
            overrun: Duration::seconds(0),
        };
        assert_eq!(idle_monitor.refresh_idle_info(), expected_idle_info);
    }

    #[test]
    fn reaches_reset_threshold() {
        let current_time = Utc::now();
        let idle_checker = make_idle_checker(1);
        let clock = make_clock(&current_time);

        let mut idle_monitor = IdleMonitor {
            idle_checker,
            clock,
            last_idle_info: IdleInfo {
                idle_since_seconds: 0,
                last_checked: current_time - Duration::milliseconds(1_025),
                last_mode_state: ModeState::Normal {
                    progress_towards_break: Duration::milliseconds(6_000),
                    progress_towards_reset: Duration::milliseconds(
                        DEFAULT_BREAK_LENGTH_SECS * 1_000 - 0_050,
                    ),
                    idle_state: DebouncedIdleState::Idle {
                        idle_since: current_time - Duration::milliseconds(5_000),
                    },
                },
                presence_mode: PresenceMode::Active,
                reading_mode: false,
                time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
                break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
                overrun: Duration::seconds(1),
            },
        };
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 1,
            last_checked: current_time,
            last_mode_state: ModeState::Normal {
                progress_towards_break: Duration::seconds(0),
                progress_towards_reset: Duration::seconds(DEFAULT_BREAK_LENGTH_SECS),
                idle_state: DebouncedIdleState::Idle {
                    idle_since: current_time - Duration::milliseconds(5_000),
                },
            },
            presence_mode: PresenceMode::Active,
            reading_mode: false,
            time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
            break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
            overrun: Duration::seconds(0),
        };
        assert_eq!(idle_monitor.refresh_idle_info(), expected_idle_info);
    }

    #[test]
    fn start_mute_in_normal() {
        let current_time = Utc::now();
        let idle_checker = make_idle_checker(1);
        let clock = make_clock(&current_time);

        let mut idle_monitor = IdleMonitor {
            idle_checker,
            clock,
            last_idle_info: IdleInfo {
                idle_since_seconds: 0,
                last_checked: current_time - Duration::milliseconds(1_025),
                last_mode_state: ModeState::Normal {
                    progress_towards_break: Duration::milliseconds(6_000),
                    progress_towards_reset: Duration::milliseconds(2_000),
                    idle_state: DebouncedIdleState::Idle {
                        idle_since: current_time - Duration::milliseconds(5_000),
                    },
                },
                presence_mode: PresenceMode::Active,
                reading_mode: false,
                time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
                break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
                overrun: Duration::seconds(0),
            },
        };
        let resume_at_stamp = current_time + Duration::seconds(5 * 60);
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 0,
            last_checked: current_time - Duration::milliseconds(1_025),
            last_mode_state: ModeState::Normal {
                progress_towards_break: Duration::milliseconds(6_000),
                progress_towards_reset: Duration::milliseconds(2_000),
                idle_state: DebouncedIdleState::Idle {
                    idle_since: current_time - Duration::milliseconds(5_000),
                },
            },
            presence_mode: PresenceMode::SnoozedUntil(resume_at_stamp),
            reading_mode: false,
            time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
            break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
            overrun: Duration::seconds(0),
        };
        assert_eq!(idle_monitor.snooze(resume_at_stamp), expected_idle_info);
    }

    #[test]
    fn remain_muted() {
        let current_time = Utc::now();
        let idle_checker = make_idle_checker(6);
        let clock = make_clock(&current_time);

        let mut idle_monitor = IdleMonitor {
            idle_checker,
            clock,
            last_idle_info: IdleInfo {
                idle_since_seconds: 5,
                last_checked: current_time - Duration::milliseconds(1_025),
                last_mode_state: ModeState::Normal {
                    progress_towards_break: Duration::milliseconds(0),
                    progress_towards_reset: Duration::milliseconds(5_000),
                    idle_state: DebouncedIdleState::Idle {
                        idle_since: current_time - Duration::milliseconds(5_000),
                    },
                },
                presence_mode: PresenceMode::SnoozedUntil(current_time + Duration::seconds(1)),
                reading_mode: false,
                time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
                break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
                overrun: Duration::seconds(0),
            },
        };
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 6,
            last_checked: current_time,
            last_mode_state: ModeState::Normal {
                progress_towards_break: Duration::milliseconds(1_025),
                progress_towards_reset: Duration::milliseconds(6_025),
                idle_state: DebouncedIdleState::Idle {
                    idle_since: current_time - Duration::milliseconds(5_000),
                },
            },
            presence_mode: PresenceMode::SnoozedUntil(current_time + Duration::seconds(1)),
            reading_mode: false,
            time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
            break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
            overrun: Duration::seconds(0),
        };
        assert_eq!(idle_monitor.refresh_idle_info(), expected_idle_info);
    }

    #[test]
    fn mute_cancels_prebreak() {
        let current_time = Utc::now();
        let idle_checker = make_idle_checker(6);
        let clock = make_clock(&current_time);

        let mut idle_monitor = IdleMonitor {
            idle_checker,
            clock,
            last_idle_info: IdleInfo {
                idle_since_seconds: 5,
                last_checked: current_time - Duration::milliseconds(1_025),
                last_mode_state: ModeState::PreBreak {
                    started_at: current_time - Duration::seconds(1),
                },
                presence_mode: PresenceMode::Active,
                reading_mode: false,
                time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
                break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
                overrun: Duration::seconds(0),
            },
        };
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 6,
            last_checked: current_time,
            last_mode_state: ModeState::Normal {
                progress_towards_break: Duration::seconds(DEFAULT_TIME_TO_BREAK_SECS),
                progress_towards_reset: Duration::milliseconds(0),
                idle_state: DebouncedIdleState::ActiveGoingToIdle {
                    active_since: current_time,
                    transitioning_since: current_time,
                },
            },
            presence_mode: PresenceMode::Muted,
            reading_mode: false,
            time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
            break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
            overrun: Duration::seconds(0),
        };
        idle_monitor.mute();
        assert_eq!(idle_monitor.refresh_idle_info(), expected_idle_info);
    }

    #[test]
    fn unmute_when_muted() {
        let current_time = Utc::now();
        let idle_checker = make_idle_checker(6);
        let clock = make_clock(&current_time);

        let mut idle_monitor = IdleMonitor {
            idle_checker,
            clock,
            last_idle_info: IdleInfo {
                idle_since_seconds: 5,
                last_checked: current_time - Duration::milliseconds(1_025),
                last_mode_state: ModeState::Normal {
                    progress_towards_break: Duration::milliseconds(0),
                    progress_towards_reset: Duration::milliseconds(5_000),
                    idle_state: DebouncedIdleState::Idle {
                        idle_since: current_time - Duration::milliseconds(5_000),
                    },
                },
                presence_mode: PresenceMode::SnoozedUntil(current_time + Duration::seconds(1)),
                reading_mode: false,
                time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
                break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
                overrun: Duration::seconds(0),
            },
        };
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 5,
            last_checked: current_time - Duration::milliseconds(1_025),
            last_mode_state: ModeState::Normal {
                progress_towards_break: Duration::milliseconds(0),
                progress_towards_reset: Duration::milliseconds(5_000),
                idle_state: DebouncedIdleState::Idle {
                    idle_since: current_time - Duration::milliseconds(5_000),
                },
            },
            presence_mode: PresenceMode::Active,
            reading_mode: false,
            time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
            break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
            overrun: Duration::seconds(0),
        };
        assert_eq!(idle_monitor.unmute(), expected_idle_info);
    }

    #[test]
    fn unmute_when_not_muted() {
        let current_time = Utc::now();
        let idle_checker = make_idle_checker(6);
        let clock = make_clock(&current_time);

        let mut idle_monitor = IdleMonitor {
            idle_checker,
            clock,
            last_idle_info: IdleInfo {
                idle_since_seconds: 5,
                last_checked: current_time - Duration::milliseconds(1_025),
                last_mode_state: ModeState::Normal {
                    progress_towards_break: Duration::milliseconds(0),
                    progress_towards_reset: Duration::milliseconds(5_000),
                    idle_state: DebouncedIdleState::Idle {
                        idle_since: current_time - Duration::milliseconds(5_000),
                    },
                },
                presence_mode: PresenceMode::Active,
                reading_mode: false,
                time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
                break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
                overrun: Duration::seconds(0),
            },
        };
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 5,
            last_checked: current_time - Duration::milliseconds(1_025),
            last_mode_state: ModeState::Normal {
                progress_towards_break: Duration::milliseconds(0),
                progress_towards_reset: Duration::milliseconds(5_000),
                idle_state: DebouncedIdleState::Idle {
                    idle_since: current_time - Duration::milliseconds(5_000),
                },
            },
            presence_mode: PresenceMode::Active,
            reading_mode: false,
            time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
            break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
            overrun: Duration::seconds(0),
        };
        assert_eq!(idle_monitor.unmute(), expected_idle_info);
    }

    #[test]
    fn wake_from_mute() {
        let current_time = Utc::now();
        let idle_checker = make_idle_checker(6);
        let clock = make_clock(&current_time);

        let mut idle_monitor = IdleMonitor {
            idle_checker,
            clock,
            last_idle_info: IdleInfo {
                idle_since_seconds: 5,
                last_checked: current_time - Duration::milliseconds(1_025),
                last_mode_state: ModeState::Normal {
                    progress_towards_break: Duration::milliseconds(0),
                    progress_towards_reset: Duration::milliseconds(5_000),
                    idle_state: DebouncedIdleState::Idle {
                        idle_since: current_time - Duration::milliseconds(5_000),
                    },
                },
                presence_mode: PresenceMode::SnoozedUntil(current_time - Duration::seconds(1)),
                reading_mode: false,
                time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
                break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
                overrun: Duration::seconds(0),
            },
        };
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 6,
            last_checked: current_time,
            last_mode_state: ModeState::Normal {
                progress_towards_break: Duration::milliseconds(1_025),
                progress_towards_reset: Duration::milliseconds(6_025),
                idle_state: DebouncedIdleState::Idle {
                    idle_since: current_time - Duration::milliseconds(5_000),
                },
            },
            presence_mode: PresenceMode::Active,
            reading_mode: false,
            time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
            break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
            overrun: Duration::seconds(0),
        };
        assert_eq!(idle_monitor.refresh_idle_info(), expected_idle_info);
    }

    #[test]
    fn reaches_break_threshold() {
        let current_time = Utc::now();
        let idle_checker = make_idle_checker(0);
        let clock = make_clock(&current_time);

        let mut idle_monitor = IdleMonitor {
            idle_checker,
            clock,
            last_idle_info: IdleInfo {
                idle_since_seconds: 0,
                last_checked: current_time - Duration::milliseconds(1_009),
                last_mode_state: ModeState::Normal {
                    progress_towards_break: Duration::milliseconds(
                        DEFAULT_TIME_TO_BREAK_SECS * 1_000 - 0_089,
                    ),
                    progress_towards_reset: Duration::seconds(0),
                    idle_state: DebouncedIdleState::Active {
                        active_since: current_time - Duration::milliseconds(10_000),
                    },
                },
                presence_mode: PresenceMode::Active,
                reading_mode: false,
                time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
                break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
                overrun: Duration::seconds(0),
            },
        };
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 0,
            last_checked: current_time,
            last_mode_state: ModeState::PreBreak {
                started_at: current_time,
            },
            presence_mode: PresenceMode::Active,
            reading_mode: false,
            time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
            break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
            overrun: Duration::seconds(0),
        };
        assert_eq!(idle_monitor.refresh_idle_info(), expected_idle_info);
    }

    #[test]
    fn ignore_break_threshold_in_mute() {
        let current_time = Utc::now();
        let idle_checker = make_idle_checker(0);
        let clock = make_clock(&current_time);

        let mut idle_monitor = IdleMonitor {
            idle_checker,
            clock,
            last_idle_info: IdleInfo {
                idle_since_seconds: 0,
                last_checked: current_time - Duration::milliseconds(1_009),
                last_mode_state: ModeState::Normal {
                    progress_towards_break: Duration::milliseconds(
                        DEFAULT_TIME_TO_BREAK_SECS * 1_000 - 0_089,
                    ),
                    progress_towards_reset: Duration::seconds(0),
                    idle_state: DebouncedIdleState::Active {
                        active_since: current_time - Duration::milliseconds(10_000),
                    },
                },
                presence_mode: PresenceMode::SnoozedUntil(current_time + Duration::seconds(1)),
                reading_mode: false,
                time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
                break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
                overrun: Duration::seconds(0),
            },
        };
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 0,
            last_checked: current_time,
            last_mode_state: ModeState::Normal {
                progress_towards_break: Duration::seconds(DEFAULT_TIME_TO_BREAK_SECS),
                progress_towards_reset: Duration::seconds(0),
                idle_state: DebouncedIdleState::Active {
                    active_since: current_time - Duration::milliseconds(10_000),
                },
            },
            presence_mode: PresenceMode::SnoozedUntil(current_time + Duration::seconds(1)),
            reading_mode: false,
            time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
            break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
            overrun: Duration::seconds(0),
        };
        assert_eq!(idle_monitor.refresh_idle_info(), expected_idle_info);
    }

    #[test]
    fn accumulate_overrun_when_progress_towards_break_full_and_in_mute() {
        let current_time = Utc::now();
        let idle_checker = make_idle_checker(0);
        let clock = make_clock(&current_time);

        let mut idle_monitor = IdleMonitor {
            idle_checker,
            clock,
            last_idle_info: IdleInfo {
                idle_since_seconds: 0,
                last_checked: current_time - Duration::milliseconds(1_009),
                last_mode_state: ModeState::Normal {
                    progress_towards_break: Duration::seconds(DEFAULT_TIME_TO_BREAK_SECS),
                    progress_towards_reset: Duration::seconds(0),
                    idle_state: DebouncedIdleState::Active {
                        active_since: current_time - Duration::milliseconds(10_000),
                    },
                },
                presence_mode: PresenceMode::SnoozedUntil(current_time + Duration::minutes(20)),
                reading_mode: false,
                time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
                break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
                overrun: Duration::seconds(0),
            },
        };
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 0,
            last_checked: current_time,
            last_mode_state: ModeState::Normal {
                progress_towards_break: Duration::seconds(DEFAULT_TIME_TO_BREAK_SECS),
                progress_towards_reset: Duration::seconds(0),
                idle_state: DebouncedIdleState::Active {
                    active_since: current_time - Duration::milliseconds(10_000),
                },
            },
            presence_mode: PresenceMode::SnoozedUntil(current_time + Duration::minutes(20)),
            reading_mode: false,
            time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
            break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
            overrun: Duration::milliseconds(1_009),
        };
        assert_eq!(idle_monitor.refresh_idle_info(), expected_idle_info);
    }

    #[test]
    fn prebreak_status_quo() {
        let current_time = Utc::now();
        let idle_checker = make_idle_checker(1);
        let clock = make_clock(&current_time);

        let mut idle_monitor = IdleMonitor {
            idle_checker,
            clock,
            last_idle_info: IdleInfo {
                idle_since_seconds: 0,
                last_checked: current_time - Duration::milliseconds(1_025),
                last_mode_state: ModeState::PreBreak {
                    started_at: current_time - Duration::seconds(5),
                },
                presence_mode: PresenceMode::Active,
                reading_mode: false,
                time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
                break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
                overrun: Duration::seconds(0),
            },
        };
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 1,
            last_checked: current_time,
            last_mode_state: ModeState::PreBreak {
                started_at: current_time - Duration::seconds(5),
            },
            presence_mode: PresenceMode::Active,
            reading_mode: false,
            time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
            break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
            overrun: Duration::milliseconds(1_025),
        };
        assert_eq!(idle_monitor.refresh_idle_info(), expected_idle_info);
    }

    #[test]
    fn prebreak_idle_reset() {
        let current_time = Utc::now();
        let idle_checker = make_idle_checker(0);
        let clock = make_clock(&current_time);

        let mut idle_monitor = IdleMonitor {
            idle_checker,
            clock,
            last_idle_info: IdleInfo {
                idle_since_seconds: 4,
                last_checked: current_time - Duration::milliseconds(1_025),
                last_mode_state: ModeState::PreBreak {
                    started_at: current_time - Duration::seconds(5),
                },
                presence_mode: PresenceMode::Active,
                reading_mode: false,
                time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
                break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
                overrun: Duration::milliseconds(4_000),
            },
        };
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 0,
            last_checked: current_time,
            last_mode_state: ModeState::PreBreak {
                started_at: current_time - Duration::seconds(5),
            },
            presence_mode: PresenceMode::Active,
            reading_mode: false,
            time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
            break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
            overrun: Duration::milliseconds(5_025),
        };
        assert_eq!(idle_monitor.refresh_idle_info(), expected_idle_info);
    }

    #[test]
    fn prebreak_idle_requirement_satisfied() {
        let current_time = Utc::now();
        let idle_checker = make_idle_checker(5);
        let clock = make_clock(&current_time);

        let mut idle_monitor = IdleMonitor {
            idle_checker,
            clock,
            last_idle_info: IdleInfo {
                idle_since_seconds: 4,
                last_checked: current_time - Duration::milliseconds(1_025),
                last_mode_state: ModeState::PreBreak {
                    started_at: current_time - Duration::seconds(5),
                },
                presence_mode: PresenceMode::Active,
                reading_mode: false,
                time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
                break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
                overrun: Duration::milliseconds(4_000),
            },
        };
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 5,
            last_checked: current_time,
            last_mode_state: ModeState::Break {
                progress_towards_finish: Duration::seconds(0),
                idle_state: DebouncedIdleState::Idle {
                    idle_since: current_time - Duration::milliseconds(5_000),
                },
            },
            presence_mode: PresenceMode::Active,
            reading_mode: false,
            time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
            break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
            overrun: Duration::milliseconds(5_025),
        };
        assert_eq!(idle_monitor.refresh_idle_info(), expected_idle_info);
    }

    #[test]
    fn break_status_quo() {
        let current_time = Utc::now();
        let idle_checker = make_idle_checker(2);
        let clock = make_clock(&current_time);

        let mut idle_monitor = IdleMonitor {
            idle_checker,
            clock,
            last_idle_info: IdleInfo {
                idle_since_seconds: 1,
                last_checked: current_time - Duration::milliseconds(1_025),
                last_mode_state: ModeState::Break {
                    progress_towards_finish: Duration::milliseconds(6_000),
                    idle_state: DebouncedIdleState::Idle {
                        idle_since: current_time - Duration::milliseconds(2_000),
                    },
                },
                presence_mode: PresenceMode::Active,
                reading_mode: false,
                time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
                break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
                overrun: Duration::milliseconds(3_000),
            },
        };
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 2,
            last_checked: current_time,
            last_mode_state: ModeState::Break {
                progress_towards_finish: Duration::milliseconds(7_025),
                idle_state: DebouncedIdleState::Idle {
                    idle_since: current_time - Duration::milliseconds(2_000),
                },
            },
            presence_mode: PresenceMode::Active,
            reading_mode: false,
            time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
            break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
            overrun: Duration::milliseconds(3_000),
        };
        assert_eq!(idle_monitor.refresh_idle_info(), expected_idle_info);
    }

    #[test]
    fn break_active_to_idle_in_reading_mode() {
        let current_time = Utc::now();
        let idle_checker = make_idle_checker(1);
        let clock = make_clock(&current_time);

        let mut idle_monitor = IdleMonitor {
            idle_checker,
            clock,
            last_idle_info: IdleInfo {
                idle_since_seconds: 0,
                last_checked: current_time - Duration::milliseconds(1_025),
                last_mode_state: ModeState::Break {
                    progress_towards_finish: Duration::milliseconds(6_000),
                    idle_state: DebouncedIdleState::Active {
                        active_since: current_time - Duration::milliseconds(2_000),
                    },
                },
                presence_mode: PresenceMode::Active,
                reading_mode: true,
                time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
                break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
                overrun: Duration::milliseconds(3_000),
            },
        };
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 1,
            last_checked: current_time,
            last_mode_state: ModeState::Break {
                progress_towards_finish: Duration::milliseconds(6_000),
                idle_state: DebouncedIdleState::ActiveGoingToIdle {
                    active_since: current_time - Duration::milliseconds(2_000),
                    transitioning_since: current_time,
                },
            },
            presence_mode: PresenceMode::Active,
            reading_mode: true,
            time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
            break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
            overrun: Duration::milliseconds(4_025),
        };
        assert_eq!(idle_monitor.refresh_idle_info(), expected_idle_info);
    }

    #[test]
    fn active_during_break_increases_overrun() {
        let current_time = Utc::now();
        let idle_checker = make_idle_checker(0);
        let clock = make_clock(&current_time);

        let mut idle_monitor = IdleMonitor {
            idle_checker,
            clock,
            last_idle_info: IdleInfo {
                idle_since_seconds: 1,
                last_checked: current_time - Duration::milliseconds(1_025),
                last_mode_state: ModeState::Break {
                    progress_towards_finish: Duration::milliseconds(6_000),
                    idle_state: DebouncedIdleState::Active {
                        active_since: current_time - Duration::milliseconds(2_000),
                    },
                },
                presence_mode: PresenceMode::Active,
                reading_mode: false,
                time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
                break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
                overrun: Duration::milliseconds(3_000),
            },
        };
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 0,
            last_checked: current_time,
            last_mode_state: ModeState::Break {
                progress_towards_finish: Duration::milliseconds(6_000),
                idle_state: DebouncedIdleState::Active {
                    active_since: current_time - Duration::milliseconds(2_000),
                },
            },
            presence_mode: PresenceMode::Active,
            reading_mode: false,
            time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
            break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
            overrun: Duration::milliseconds(4_025),
        };
        assert_eq!(idle_monitor.refresh_idle_info(), expected_idle_info);
    }

    #[test]
    fn break_concluded() {
        let current_time = Utc::now();
        let idle_checker = make_idle_checker(28);
        let clock = make_clock(&current_time);

        let mut idle_monitor = IdleMonitor {
            idle_checker,
            clock,
            last_idle_info: IdleInfo {
                idle_since_seconds: 27,
                last_checked: current_time - Duration::milliseconds(1_025),
                last_mode_state: ModeState::Break {
                    progress_towards_finish: Duration::milliseconds(
                        DEFAULT_BREAK_LENGTH_SECS * 1_000 - 0_052,
                    ),
                    idle_state: DebouncedIdleState::Idle {
                        idle_since: current_time - Duration::milliseconds(28_000),
                    },
                },
                presence_mode: PresenceMode::Active,
                reading_mode: false,
                time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
                break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
                overrun: Duration::milliseconds(3_000),
            },
        };
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 28,
            last_checked: current_time,
            last_mode_state: ModeState::Normal {
                progress_towards_break: Duration::seconds(0),
                progress_towards_reset: Duration::seconds(DEFAULT_BREAK_LENGTH_SECS),
                idle_state: DebouncedIdleState::Idle {
                    idle_since: current_time - Duration::milliseconds(28_000),
                },
            },
            presence_mode: PresenceMode::Active,
            reading_mode: false,
            time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
            break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
            overrun: Duration::milliseconds(0_000),
        };
        assert_eq!(idle_monitor.refresh_idle_info(), expected_idle_info);
    }

    #[test]
    fn force_break() {
        let current_time = Utc::now();
        let idle_checker = make_idle_checker(0);
        let clock = make_clock(&current_time);

        let mut idle_monitor = IdleMonitor {
            idle_checker,
            clock,
            last_idle_info: IdleInfo {
                idle_since_seconds: 0,
                last_checked: current_time,
                last_mode_state: ModeState::Normal {
                    progress_towards_break: Duration::milliseconds(8_000),
                    progress_towards_reset: Duration::seconds(0),
                    idle_state: DebouncedIdleState::Active {
                        active_since: current_time,
                    },
                },
                presence_mode: PresenceMode::Active,
                reading_mode: false,
                time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
                break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
                overrun: Duration::milliseconds(0_000),
            },
        };
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 0,
            last_checked: current_time,
            last_mode_state: ModeState::Break {
                progress_towards_finish: Duration::seconds(0),
                // Forced idle transition to avoid "break interrupted"
                idle_state: DebouncedIdleState::Idle {
                    idle_since: current_time,
                },
            },
            presence_mode: PresenceMode::Active,
            reading_mode: false,
            time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
            break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
            overrun: Duration::milliseconds(0_000),
        };
        assert_eq!(idle_monitor.trigger_break(), expected_idle_info);
    }

    #[test]
    fn start_mute_in_break() {
        let current_time = Utc::now();
        let idle_checker = make_idle_checker(1);
        let clock = make_clock(&current_time);

        let mut idle_monitor = IdleMonitor {
            idle_checker,
            clock,
            last_idle_info: IdleInfo {
                idle_since_seconds: 0,
                last_checked: current_time - Duration::milliseconds(1_025),
                last_mode_state: ModeState::Break {
                    progress_towards_finish: Duration::milliseconds(6_000),
                    idle_state: DebouncedIdleState::Idle {
                        idle_since: current_time - Duration::milliseconds(2_000),
                    },
                },
                presence_mode: PresenceMode::Active,
                reading_mode: false,
                time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
                break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
                overrun: Duration::milliseconds(0_000),
            },
        };
        let resume_at_stamp = current_time + Duration::seconds(5 * 60);
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 0,
            last_checked: current_time - Duration::milliseconds(1_025),
            last_mode_state: ModeState::Break {
                progress_towards_finish: Duration::milliseconds(6_000),
                idle_state: DebouncedIdleState::Idle {
                    idle_since: current_time - Duration::milliseconds(2_000),
                },
            },
            presence_mode: PresenceMode::SnoozedUntil(resume_at_stamp),
            reading_mode: false,
            time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
            break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
            overrun: Duration::milliseconds(0_000),
        };
        assert_eq!(idle_monitor.snooze(resume_at_stamp), expected_idle_info);
    }

    #[test]
    fn skip_break() {
        let current_time = Utc::now();
        let idle_checker = make_idle_checker(0);
        let clock = make_clock(&current_time);

        let mut idle_monitor = IdleMonitor {
            idle_checker,
            clock,
            last_idle_info: IdleInfo {
                idle_since_seconds: 0,
                last_checked: current_time - Duration::milliseconds(1_025),
                last_mode_state: ModeState::Break {
                    progress_towards_finish: Duration::milliseconds(6_000),
                    idle_state: DebouncedIdleState::Active {
                        active_since: current_time - Duration::milliseconds(2_000),
                    },
                },
                presence_mode: PresenceMode::Active,
                reading_mode: false,
                time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
                break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
                overrun: Duration::milliseconds(8_000),
            },
        };
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 0,
            last_checked: current_time,
            last_mode_state: ModeState::Normal {
                progress_towards_break: Duration::seconds(0),
                progress_towards_reset: Duration::seconds(0),
                idle_state: DebouncedIdleState::Active {
                    active_since: current_time - Duration::milliseconds(2_000),
                },
            },
            presence_mode: PresenceMode::Active,
            reading_mode: false,
            time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
            break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
            overrun: Duration::milliseconds(0_000),
        };
        assert_eq!(idle_monitor.skip_break(), expected_idle_info);
    }

    #[test]
    fn postpone_break() {
        let current_time = Utc::now();
        let idle_checker = make_idle_checker(0);
        let clock = make_clock(&current_time);

        let mut idle_monitor = IdleMonitor {
            idle_checker,
            clock,
            last_idle_info: IdleInfo {
                idle_since_seconds: 0,
                last_checked: current_time - Duration::milliseconds(1_025),
                last_mode_state: ModeState::Break {
                    progress_towards_finish: Duration::milliseconds(6_000),
                    idle_state: DebouncedIdleState::Active {
                        active_since: current_time - Duration::milliseconds(2_000),
                    },
                },
                presence_mode: PresenceMode::Active,
                reading_mode: false,
                time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
                break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
                overrun: Duration::milliseconds(8_000),
            },
        };
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 0,
            last_checked: current_time,
            last_mode_state: ModeState::Normal {
                progress_towards_break: Duration::seconds(DEFAULT_TIME_TO_BREAK_SECS - (3 * 60)),
                progress_towards_reset: Duration::seconds(0),
                idle_state: DebouncedIdleState::Active {
                    active_since: current_time - Duration::milliseconds(2_000),
                },
            },
            presence_mode: PresenceMode::Active,
            reading_mode: false,
            time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
            break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
            overrun: Duration::milliseconds(8_000),
        };
        assert_eq!(
            idle_monitor.postpone_break(Duration::seconds(3 * 60)),
            expected_idle_info
        );
    }

    #[test]
    fn set_reading_mode_normal_active() {
        let current_time = Utc::now();
        let idle_checker = make_idle_checker(0);
        let clock = make_clock(&current_time);

        let mut idle_monitor = IdleMonitor {
            idle_checker,
            clock,
            last_idle_info: IdleInfo {
                idle_since_seconds: 0,
                last_checked: current_time,
                last_mode_state: ModeState::Normal {
                    progress_towards_break: Duration::milliseconds(6_000),
                    progress_towards_reset: Duration::milliseconds(0_000),
                    idle_state: DebouncedIdleState::Active {
                        active_since: current_time - Duration::milliseconds(2_000),
                    },
                },
                presence_mode: PresenceMode::Active,
                reading_mode: false,
                time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
                break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
                overrun: Duration::milliseconds(0_000),
            },
        };
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 0,
            last_checked: current_time,
            last_mode_state: ModeState::Normal {
                progress_towards_break: Duration::milliseconds(6_000),
                progress_towards_reset: Duration::milliseconds(0_000),
                idle_state: DebouncedIdleState::Active {
                    active_since: current_time - Duration::milliseconds(2_000),
                },
            },
            presence_mode: PresenceMode::Active,
            reading_mode: true,
            time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
            break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
            overrun: Duration::milliseconds(0_000),
        };
        idle_monitor.set_reading_mode(true);
        assert_eq!(idle_monitor.get_last_idle_info(), expected_idle_info);
    }

    #[test]
    fn set_reading_mode_normal_idle() {
        let current_time = Utc::now();
        let idle_checker = make_idle_checker(0);
        let clock = make_clock(&current_time);

        let mut idle_monitor = IdleMonitor {
            idle_checker,
            clock,
            last_idle_info: IdleInfo {
                idle_since_seconds: 0,
                last_checked: current_time,
                last_mode_state: ModeState::Normal {
                    progress_towards_break: Duration::milliseconds(6_000),
                    progress_towards_reset: Duration::milliseconds(0_000),
                    idle_state: DebouncedIdleState::Idle {
                        idle_since: current_time - Duration::milliseconds(2_000),
                    },
                },
                presence_mode: PresenceMode::Active,
                reading_mode: false,
                time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
                break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
                overrun: Duration::milliseconds(0_000),
            },
        };
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 0,
            last_checked: current_time,
            last_mode_state: ModeState::Normal {
                progress_towards_break: Duration::milliseconds(6_000),
                progress_towards_reset: Duration::milliseconds(0_000),
                idle_state: DebouncedIdleState::Active {
                    active_since: current_time,
                },
            },
            presence_mode: PresenceMode::Active,
            reading_mode: true,
            time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
            break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
            overrun: Duration::milliseconds(0_000),
        };
        idle_monitor.set_reading_mode(true);
        assert_eq!(idle_monitor.get_last_idle_info(), expected_idle_info);
    }

    #[test]
    fn set_reading_mode_prebreak() {
        let current_time = Utc::now();
        let idle_checker = make_idle_checker(0);
        let clock = make_clock(&current_time);

        let mut idle_monitor = IdleMonitor {
            idle_checker,
            clock,
            last_idle_info: IdleInfo {
                idle_since_seconds: 0,
                last_checked: current_time,
                last_mode_state: ModeState::PreBreak {
                    started_at: current_time - Duration::seconds(3),
                },
                presence_mode: PresenceMode::Active,
                reading_mode: false,
                time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
                break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
                overrun: Duration::milliseconds(0_000),
            },
        };
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 0,
            last_checked: current_time,
            last_mode_state: ModeState::PreBreak {
                started_at: current_time - Duration::seconds(3),
            },
            presence_mode: PresenceMode::Active,
            reading_mode: true,
            time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
            break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
            overrun: Duration::milliseconds(0_000),
        };
        idle_monitor.set_reading_mode(true);
        assert_eq!(idle_monitor.get_last_idle_info(), expected_idle_info);
    }

    #[test]
    fn set_reading_mode_break_active() {
        let current_time = Utc::now();
        let idle_checker = make_idle_checker(0);
        let clock = make_clock(&current_time);

        let mut idle_monitor = IdleMonitor {
            idle_checker,
            clock,
            last_idle_info: IdleInfo {
                idle_since_seconds: 0,
                last_checked: current_time,
                last_mode_state: ModeState::Break {
                    progress_towards_finish: Duration::milliseconds(6_000),
                    idle_state: DebouncedIdleState::Active {
                        active_since: current_time - Duration::milliseconds(2_000),
                    },
                },
                presence_mode: PresenceMode::Active,
                reading_mode: false,
                time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
                break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
                overrun: Duration::milliseconds(0_000),
            },
        };
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 0,
            last_checked: current_time,
            last_mode_state: ModeState::Break {
                progress_towards_finish: Duration::milliseconds(6_000),
                idle_state: DebouncedIdleState::Active {
                    active_since: current_time - Duration::milliseconds(2_000),
                },
            },
            presence_mode: PresenceMode::Active,
            reading_mode: true,
            time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
            break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
            overrun: Duration::milliseconds(0_000),
        };
        idle_monitor.set_reading_mode(true);
        assert_eq!(idle_monitor.get_last_idle_info(), expected_idle_info);
    }

    #[test]
    fn set_reading_mode_break_idle() {
        let current_time = Utc::now();
        let idle_checker = make_idle_checker(0);
        let clock = make_clock(&current_time);

        let mut idle_monitor = IdleMonitor {
            idle_checker,
            clock,
            last_idle_info: IdleInfo {
                idle_since_seconds: 0,
                last_checked: current_time,
                last_mode_state: ModeState::Break {
                    progress_towards_finish: Duration::milliseconds(6_000),
                    idle_state: DebouncedIdleState::Idle {
                        idle_since: current_time - Duration::milliseconds(2_000),
                    },
                },
                presence_mode: PresenceMode::Active,
                reading_mode: false,
                time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
                break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
                overrun: Duration::milliseconds(0_000),
            },
        };
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 0,
            last_checked: current_time,
            last_mode_state: ModeState::Break {
                progress_towards_finish: Duration::milliseconds(6_000),
                idle_state: DebouncedIdleState::Active {
                    active_since: current_time,
                },
            },
            presence_mode: PresenceMode::Active,
            reading_mode: true,
            time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
            break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
            overrun: Duration::milliseconds(0_000),
        };
        idle_monitor.set_reading_mode(true);
        assert_eq!(idle_monitor.get_last_idle_info(), expected_idle_info);
    }

    #[test]
    fn change_time_to_break_no_clamping() {
        let current_time = Utc::now();
        let idle_checker = make_idle_checker(1);
        let clock = make_clock(&current_time);

        let mut idle_monitor = IdleMonitor {
            idle_checker,
            clock,
            last_idle_info: IdleInfo {
                idle_since_seconds: 0,
                last_checked: current_time,
                last_mode_state: ModeState::Normal {
                    progress_towards_break: Duration::seconds(8),
                    progress_towards_reset: Duration::seconds(2),
                    idle_state: DebouncedIdleState::Idle {
                        idle_since: current_time - Duration::seconds(5),
                    },
                },
                presence_mode: PresenceMode::Active,
                reading_mode: false,
                time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
                break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
                overrun: Duration::milliseconds(0_000),
            },
        };
        idle_monitor.set_time_to_break(600);
        assert_eq!(
            idle_monitor.get_last_idle_info(),
            IdleInfo {
                idle_since_seconds: 0,
                last_checked: current_time,
                last_mode_state: ModeState::Normal {
                    progress_towards_break: Duration::seconds(8),
                    progress_towards_reset: Duration::seconds(2),
                    idle_state: DebouncedIdleState::Idle {
                        idle_since: current_time - Duration::seconds(5),
                    },
                },
                presence_mode: PresenceMode::Active,
                reading_mode: false,
                time_to_break_secs: 600,
                break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
                overrun: Duration::milliseconds(0_000),
            }
        );
    }

    #[test]
    fn change_time_to_break_needs_clamping() {
        let current_time = Utc::now();
        let idle_checker = make_idle_checker(1);
        let clock = make_clock(&current_time);

        let mut idle_monitor = IdleMonitor {
            idle_checker,
            clock,
            last_idle_info: IdleInfo {
                idle_since_seconds: 0,
                last_checked: current_time,
                last_mode_state: ModeState::Normal {
                    progress_towards_break: Duration::seconds(800),
                    progress_towards_reset: Duration::seconds(2),
                    idle_state: DebouncedIdleState::Idle {
                        idle_since: current_time - Duration::seconds(5),
                    },
                },
                presence_mode: PresenceMode::Active,
                reading_mode: false,
                time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
                break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
                overrun: Duration::milliseconds(0_000),
            },
        };
        idle_monitor.set_time_to_break(600);
        assert_eq!(
            idle_monitor.get_last_idle_info(),
            IdleInfo {
                idle_since_seconds: 0,
                last_checked: current_time,
                last_mode_state: ModeState::Normal {
                    progress_towards_break: Duration::seconds(600),
                    progress_towards_reset: Duration::seconds(2),
                    idle_state: DebouncedIdleState::Idle {
                        idle_since: current_time - Duration::seconds(5),
                    },
                },
                presence_mode: PresenceMode::Active,
                reading_mode: false,
                time_to_break_secs: 600,
                break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
                overrun: Duration::milliseconds(0_000),
            }
        );
    }

    #[test]
    fn change_break_length_no_clamping() {
        let current_time = Utc::now();
        let idle_checker = make_idle_checker(1);
        let clock = make_clock(&current_time);

        let mut idle_monitor = IdleMonitor {
            idle_checker,
            clock,
            last_idle_info: IdleInfo {
                idle_since_seconds: 0,
                last_checked: current_time,
                last_mode_state: ModeState::Normal {
                    progress_towards_break: Duration::seconds(2),
                    progress_towards_reset: Duration::seconds(8),
                    idle_state: DebouncedIdleState::Idle {
                        idle_since: current_time - Duration::seconds(5),
                    },
                },
                presence_mode: PresenceMode::Active,
                reading_mode: false,
                time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
                break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
                overrun: Duration::milliseconds(0_000),
            },
        };
        idle_monitor.set_break_length(600);
        assert_eq!(
            idle_monitor.get_last_idle_info(),
            IdleInfo {
                idle_since_seconds: 0,
                last_checked: current_time,
                last_mode_state: ModeState::Normal {
                    progress_towards_break: Duration::seconds(2),
                    progress_towards_reset: Duration::seconds(8),
                    idle_state: DebouncedIdleState::Idle {
                        idle_since: current_time - Duration::seconds(5),
                    },
                },
                presence_mode: PresenceMode::Active,
                reading_mode: false,
                time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
                break_length_secs: 600,
                overrun: Duration::milliseconds(0_000),
            }
        );
    }

    #[test]
    fn change_break_length_needs_clamping() {
        let current_time = Utc::now();
        let idle_checker = make_idle_checker(1);
        let clock = make_clock(&current_time);

        let mut idle_monitor = IdleMonitor {
            idle_checker,
            clock,
            last_idle_info: IdleInfo {
                idle_since_seconds: 0,
                last_checked: current_time,
                last_mode_state: ModeState::Normal {
                    progress_towards_break: Duration::seconds(2),
                    progress_towards_reset: Duration::seconds(800),
                    idle_state: DebouncedIdleState::Idle {
                        idle_since: current_time - Duration::seconds(5),
                    },
                },
                presence_mode: PresenceMode::Active,
                reading_mode: false,
                time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
                break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
                overrun: Duration::milliseconds(0_000),
            },
        };
        idle_monitor.set_break_length(600);
        assert_eq!(
            idle_monitor.get_last_idle_info(),
            IdleInfo {
                idle_since_seconds: 0,
                last_checked: current_time,
                last_mode_state: ModeState::Normal {
                    progress_towards_break: Duration::seconds(2),
                    progress_towards_reset: Duration::seconds(600),
                    idle_state: DebouncedIdleState::Idle {
                        idle_since: current_time - Duration::seconds(5),
                    },
                },
                presence_mode: PresenceMode::Active,
                reading_mode: false,
                time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
                break_length_secs: 600,
                overrun: Duration::milliseconds(0_000),
            }
        );
    }

    #[test]
    fn export_persistable_state() {
        let current_time = Utc::now();
        let idle_checker = make_idle_checker(1);
        let clock = make_clock(&current_time);

        let idle_monitor = IdleMonitor {
            idle_checker,
            clock,
            last_idle_info: IdleInfo {
                idle_since_seconds: 0,
                last_checked: current_time,
                last_mode_state: ModeState::Normal {
                    progress_towards_break: Duration::milliseconds(6_000),
                    progress_towards_reset: Duration::milliseconds(2_000),
                    idle_state: DebouncedIdleState::Idle {
                        idle_since: current_time - Duration::milliseconds(5_000),
                    },
                },
                presence_mode: PresenceMode::Active,
                reading_mode: false,
                time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
                break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
                overrun: Duration::milliseconds(0_000),
            },
        };
        assert_eq!(
            idle_monitor.export_persistable_state(),
            PersistableState {
                last_checked: current_time,
                progress_towards_break: Duration::milliseconds(6_000),
                progress_towards_reset: Duration::milliseconds(2_000),
                presence_mode: PresenceMode::Active,
                reading_mode: false,
                time_to_break_secs: DEFAULT_TIME_TO_BREAK_SECS,
                break_length_secs: DEFAULT_BREAK_LENGTH_SECS,
            }
        );
    }
}

use chrono::{DateTime, Duration, Utc};
use core::fmt;
use serde::{Deserialize, Serialize};
use user_idle2::UserIdle;

#[cfg(test)]
use mockall::automock;

pub const TIME_TO_BREAK_SECS: i64 = 20 * 60;
pub const BREAK_LENGTH_SECS: i64 = 90;
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
        muted_until: Option<DateTime<Utc>>,
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

#[derive(Debug, PartialEq, Clone, Copy, Serialize, Deserialize)]
pub struct IdleInfo {
    pub idle_since_seconds: u64,
    pub last_checked: DateTime<Utc>,
    pub last_mode_state: ModeState,
    pub reading_mode: bool,
}

impl IdleInfo {
    pub fn is_muted(&self) -> bool {
        match self.last_mode_state {
            ModeState::Normal { muted_until, .. } => muted_until.is_some(),
            _ => false,
        }
    }
}

pub struct IdleMonitor<T: AbstractIdleChecker, U: AbstractClock> {
    idle_checker: T,
    clock: U,
    last_idle_info: IdleInfo,
}

impl<T: AbstractIdleChecker, U: AbstractClock> IdleMonitor<T, U> {
    pub fn new(idle_checker: T, clock: U) -> Self {
        let time = clock.get_time();
        Self {
            idle_checker,
            clock,
            last_idle_info: IdleInfo {
                idle_since_seconds: 0,
                last_checked: time,
                last_mode_state: ModeState::Normal {
                    muted_until: None,
                    idle_state: DebouncedIdleState::Active { active_since: time },
                    progress_towards_break: Duration::seconds(0),
                    progress_towards_reset: Duration::seconds(0),
                },
                reading_mode: false,
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
                _ => match self.last_idle_info.reading_mode {
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
        muted_until: Option<DateTime<Utc>>,
        idle_since_seconds: u64,
        progress_towards_break: Duration,
        progress_towards_reset: Duration,
        prv_idle_state: DebouncedIdleState,
        check_time: DateTime<Utc>,
        new_idle_state: DebouncedIdleState,
        time_since_last_check: Duration,
        reading_mode: bool,
    ) -> IdleInfo {
        IdleInfo {
            idle_since_seconds,
            last_checked: check_time,
            last_mode_state: ModeState::Normal {
                muted_until: match muted_until {
                    Some(inner_timestamp) if inner_timestamp < check_time => None,
                    _ => muted_until,
                },
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
                        >= Duration::seconds(BREAK_LENGTH_SECS) =>
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
                    _ => Duration::seconds(TIME_TO_BREAK_SECS)
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
                    _ => Duration::seconds(BREAK_LENGTH_SECS)
                        .min(progress_towards_reset + time_since_last_check),
                },
            },
            reading_mode,
        }
    }

    fn _make_idle_info_in_prebreak_state(
        &mut self,
        idle_since_seconds: u64,
        last_checked: DateTime<Utc>,
        waiting_since: DateTime<Utc>,
        reading_mode: bool,
    ) -> IdleInfo {
        IdleInfo {
            idle_since_seconds,
            last_checked,
            last_mode_state: ModeState::PreBreak {
                started_at: waiting_since,
            },
            reading_mode,
        }
    }

    fn _make_idle_info_in_break_state(
        &mut self,
        idle_since_seconds: u64,
        last_checked: DateTime<Utc>,
        new_idle_state: DebouncedIdleState,
        progress_towards_finish: Duration,
        time_since_last_check: Duration,
        reading_mode: bool,
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
                    } => Duration::seconds(BREAK_LENGTH_SECS)
                        .min(progress_towards_finish + time_since_last_check),
                    _ => progress_towards_finish,
                },
                idle_state: new_idle_state,
            },
            reading_mode,
        }
    }

    pub fn mute_until(&mut self, timestamp: DateTime<Utc>) -> IdleInfo {
        let check_time = self.clock.get_time();

        self.last_idle_info.last_mode_state = match self.last_idle_info.last_mode_state {
            ModeState::Normal {
                muted_until: _,
                progress_towards_break,
                progress_towards_reset,
                idle_state,
            } => ModeState::Normal {
                muted_until: Some(timestamp),
                progress_towards_break,
                progress_towards_reset,
                idle_state,
            },
            _ => ModeState::Normal {
                muted_until: Some(timestamp),
                progress_towards_break: Duration::seconds(0),
                progress_towards_reset: Duration::seconds(0),
                idle_state: DebouncedIdleState::Idle {
                    idle_since: check_time,
                },
            },
        };
        self.last_idle_info.clone()
    }

    pub fn unmute(&mut self) -> IdleInfo {
        self.last_idle_info = match self.last_idle_info.last_mode_state {
            ModeState::Normal {
                muted_until: _,
                progress_towards_break,
                progress_towards_reset,
                idle_state,
            } => {
                self.last_idle_info.last_mode_state = ModeState::Normal {
                    muted_until: None,
                    progress_towards_break,
                    progress_towards_reset,
                    idle_state,
                };
                self.last_idle_info
            }
            _ => self.last_idle_info,
        };
        self.last_idle_info.clone()
    }

    pub fn set_reading_mode(&mut self, reading_mode: bool) {
        self.last_idle_info.reading_mode = reading_mode;
    }

    pub fn refresh_idle_info(&mut self) -> IdleInfo {
        let idle_since_seconds = self.idle_checker.get_idle_time_in_seconds();
        let check_time = self.clock.get_time();
        let start_of_transition_period = self.clock.get_time()
            - Duration::seconds(TRANSITION_THRESHOLD_SECS.try_into().unwrap());
        let time_since_last_check =
            check_time.signed_duration_since(self.last_idle_info.last_checked);
        let last_mode_state = self.last_idle_info.last_mode_state.clone();

        self.last_idle_info = match last_mode_state {
            ModeState::Normal {
                muted_until,
                progress_towards_break,
                progress_towards_reset: _,
                idle_state: _,
            } if progress_towards_break + time_since_last_check
                >= Duration::seconds(TIME_TO_BREAK_SECS)
                && time_since_last_check < Duration::seconds(FRAME_DROP_CUTOFF_POINT_SECS)
                && muted_until == None =>
            {
                self._make_idle_info_in_prebreak_state(
                    idle_since_seconds,
                    check_time,
                    self.clock.get_time(),
                    self.last_idle_info.reading_mode,
                )
            }
            ModeState::Normal {
                muted_until,
                progress_towards_break,
                progress_towards_reset,
                idle_state,
            } => self._make_idle_info_in_normal_state(
                muted_until,
                idle_since_seconds,
                if time_since_last_check > Duration::seconds(TIME_TO_BREAK_SECS) {
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
                ),
                time_since_last_check,
                self.last_idle_info.reading_mode,
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
                    reading_mode: self.last_idle_info.reading_mode,
                }
            }
            ModeState::PreBreak {
                started_at: waiting_since,
            } => self._make_idle_info_in_prebreak_state(
                idle_since_seconds,
                check_time,
                waiting_since,
                self.last_idle_info.reading_mode,
            ),
            ModeState::Break {
                progress_towards_finish,
                idle_state,
            } if progress_towards_finish + time_since_last_check
                >= Duration::seconds(BREAK_LENGTH_SECS) =>
            {
                IdleInfo {
                    idle_since_seconds,
                    last_checked: check_time,
                    last_mode_state: ModeState::Normal {
                        muted_until: None,
                        progress_towards_break: Duration::seconds(0),
                        progress_towards_reset: Duration::seconds(BREAK_LENGTH_SECS),
                        idle_state: self._make_debounced_idle_state(
                            idle_since_seconds,
                            idle_state,
                            time_since_last_check,
                            check_time,
                            start_of_transition_period,
                        ),
                    },
                    reading_mode: self.last_idle_info.reading_mode,
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
                ),
                progress_towards_finish,
                time_since_last_check,
                self.last_idle_info.reading_mode,
            ),
        };

        return self.last_idle_info.clone();
    }

    pub fn force_break(&mut self) -> IdleInfo {
        let check_time = self.clock.get_time();
        let idle_since_seconds = self.idle_checker.get_idle_time_in_seconds();

        self.last_idle_info = match self.last_idle_info.last_mode_state {
            ModeState::Normal {
                muted_until: _,
                progress_towards_break: _,
                progress_towards_reset: _,
                idle_state: _,
            } => IdleInfo {
                idle_since_seconds,
                last_checked: check_time,
                last_mode_state: ModeState::Break {
                    progress_towards_finish: Duration::seconds(0),
                    idle_state: DebouncedIdleState::Idle {
                        idle_since: check_time,
                    },
                },
                reading_mode: self.last_idle_info.reading_mode,
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
                    muted_until: None,
                    progress_towards_break: Duration::seconds(0),
                    progress_towards_reset: Duration::seconds(0),
                    idle_state,
                },
                reading_mode: self.last_idle_info.reading_mode,
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
                    muted_until: None,
                    progress_towards_break: Duration::seconds(TIME_TO_BREAK_SECS)
                        - postpone_duration,
                    progress_towards_reset: Duration::seconds(0),
                    idle_state,
                },
                reading_mode: self.last_idle_info.reading_mode,
            },
            _ => self.last_idle_info,
        };
        return self.last_idle_info.clone();
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
    fn test_freshly_initialized() {
        let current_time = Utc::now();
        let idle_checker = make_idle_checker(0);
        let clock = make_clock(&current_time);

        let mut idle_monitor = IdleMonitor::new(idle_checker, clock);
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 0,
            last_checked: current_time,
            last_mode_state: ModeState::Normal {
                muted_until: None,
                progress_towards_break: Duration::seconds(0),
                progress_towards_reset: Duration::seconds(0),
                idle_state: DebouncedIdleState::Active {
                    active_since: current_time,
                },
            },
            reading_mode: false,
        };
        assert_eq!(idle_monitor.refresh_idle_info(), expected_idle_info);
    }

    #[test]
    fn test_active_status_quo() {
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
                    muted_until: None,
                    progress_towards_break: Duration::milliseconds(20001),
                    progress_towards_reset: Duration::seconds(0),
                    idle_state: DebouncedIdleState::Active {
                        active_since: current_time - Duration::milliseconds(10000),
                    },
                },
                reading_mode: false,
            },
        };
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 0,
            last_checked: current_time,
            last_mode_state: ModeState::Normal {
                muted_until: None,
                progress_towards_break: Duration::milliseconds(21010),
                progress_towards_reset: Duration::seconds(0),
                idle_state: DebouncedIdleState::Active {
                    active_since: current_time - Duration::milliseconds(10000),
                },
            },
            reading_mode: false,
        };
        assert_eq!(idle_monitor.refresh_idle_info(), expected_idle_info);
    }

    #[test]
    fn test_ignore_jumps_in_last_check_time_above_cutoff_point_below_time_to_break() {
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
                    muted_until: None,
                    progress_towards_break: Duration::milliseconds(20_001),
                    progress_towards_reset: Duration::seconds(0),
                    idle_state: DebouncedIdleState::Active {
                        active_since: current_time - Duration::milliseconds(10_000),
                    },
                },
                reading_mode: false,
            },
        };
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 0,
            last_checked: current_time,
            last_mode_state: ModeState::Normal {
                muted_until: None,
                progress_towards_break: Duration::milliseconds(20_001),
                progress_towards_reset: Duration::seconds(0),
                idle_state: DebouncedIdleState::Idle {
                    idle_since: current_time,
                },
            },
            reading_mode: false,
        };
        assert_eq!(idle_monitor.refresh_idle_info(), expected_idle_info);
    }

    #[test]
    fn test_ignore_jumps_in_last_check_time_above_time_to_break() {
        // This is to handle standby
        let current_time = Utc::now();
        let idle_checker = make_idle_checker(4643);
        let clock = make_clock(&current_time);

        let mut idle_monitor = IdleMonitor {
            idle_checker,
            clock,
            last_idle_info: IdleInfo {
                idle_since_seconds: 0,
                last_checked: current_time - Duration::seconds(TIME_TO_BREAK_SECS + 1),
                last_mode_state: ModeState::Normal {
                    muted_until: None,
                    progress_towards_break: Duration::seconds(14),
                    progress_towards_reset: Duration::seconds(0),
                    idle_state: DebouncedIdleState::Active {
                        active_since: current_time - Duration::seconds(15),
                    },
                },
                reading_mode: false,
            },
        };
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 4643,
            last_checked: current_time,
            last_mode_state: ModeState::Normal {
                muted_until: None,
                progress_towards_break: Duration::seconds(0),
                progress_towards_reset: Duration::seconds(0),
                idle_state: DebouncedIdleState::Idle {
                    idle_since: current_time,
                },
            },
            reading_mode: false,
        };
        assert_eq!(idle_monitor.refresh_idle_info(), expected_idle_info);
    }

    #[test]
    fn test_idle_status_quo() {
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
                    muted_until: None,
                    progress_towards_break: Duration::milliseconds(6000),
                    progress_towards_reset: Duration::milliseconds(2000),
                    idle_state: DebouncedIdleState::Idle {
                        idle_since: current_time - Duration::milliseconds(5000),
                    },
                },
                reading_mode: false,
            },
        };
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 1,
            last_checked: current_time,
            last_mode_state: ModeState::Normal {
                muted_until: None,
                progress_towards_break: Duration::milliseconds(7025),
                progress_towards_reset: Duration::milliseconds(3025),
                idle_state: DebouncedIdleState::Idle {
                    idle_since: current_time - Duration::milliseconds(5000),
                },
            },
            reading_mode: false,
        };
        assert_eq!(idle_monitor.refresh_idle_info(), expected_idle_info);
    }

    #[test]
    fn test_idle_starting_transition() {
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
                    muted_until: None,
                    progress_towards_break: Duration::milliseconds(24_000),
                    progress_towards_reset: Duration::milliseconds(2_000),
                    idle_state: DebouncedIdleState::Idle {
                        idle_since: current_time - Duration::milliseconds(5_000),
                    },
                },
                reading_mode: false,
            },
        };
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 0,
            last_checked: current_time,
            last_mode_state: ModeState::Normal {
                muted_until: None,
                progress_towards_break: Duration::milliseconds(25_999),
                progress_towards_reset: Duration::milliseconds(3_999),
                idle_state: DebouncedIdleState::IdleGoingToActive {
                    idle_since: current_time - Duration::milliseconds(5_000),
                    transitioning_since: current_time,
                },
            },
            reading_mode: false,
        };
        assert_eq!(idle_monitor.refresh_idle_info(), expected_idle_info);
    }

    #[test]
    fn test_idle_still_transitioning() {
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
                    muted_until: None,
                    progress_towards_break: Duration::milliseconds(12_000),
                    progress_towards_reset: Duration::milliseconds(4_000),
                    idle_state: DebouncedIdleState::IdleGoingToActive {
                        idle_since: current_time - Duration::milliseconds(6_000),
                        transitioning_since: current_time - Duration::milliseconds(1_000),
                    },
                },
                reading_mode: false,
            },
        };
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 1,
            last_checked: current_time,
            last_mode_state: ModeState::Normal {
                muted_until: None,
                progress_towards_break: Duration::milliseconds(13_999),
                progress_towards_reset: Duration::milliseconds(5_999),
                idle_state: DebouncedIdleState::IdleGoingToActive {
                    idle_since: current_time - Duration::milliseconds(6_000),
                    transitioning_since: current_time - Duration::milliseconds(1_000),
                },
            },
            reading_mode: false,
        };
        assert_eq!(idle_monitor.refresh_idle_info(), expected_idle_info);
    }

    #[test]
    fn test_idle_cancelling_transition() {
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
                    muted_until: None,
                    progress_towards_break: Duration::milliseconds(8_000),
                    progress_towards_reset: Duration::milliseconds(2_012),
                    idle_state: DebouncedIdleState::IdleGoingToActive {
                        idle_since: current_time - Duration::milliseconds(8_000),
                        transitioning_since: current_time - Duration::milliseconds(3_000),
                    },
                },
                reading_mode: false,
            },
        };
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 3,
            last_checked: current_time,
            last_mode_state: ModeState::Normal {
                muted_until: None,
                progress_towards_break: Duration::milliseconds(9_000),
                progress_towards_reset: Duration::milliseconds(3_012),
                idle_state: DebouncedIdleState::Idle {
                    idle_since: current_time - Duration::milliseconds(8_000),
                },
            },
            reading_mode: false,
        };
        assert_eq!(idle_monitor.refresh_idle_info(), expected_idle_info);
    }

    #[test]
    fn test_idle_finalizing_transition() {
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
                    muted_until: None,
                    progress_towards_break: Duration::milliseconds(11_000),
                    progress_towards_reset: Duration::milliseconds(8_000),
                    idle_state: DebouncedIdleState::IdleGoingToActive {
                        idle_since: current_time - Duration::milliseconds(9_000),
                        transitioning_since: current_time - Duration::milliseconds(3_000),
                    },
                },
                reading_mode: false,
            },
        };
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 1,
            last_checked: current_time,
            last_mode_state: ModeState::Normal {
                muted_until: None,
                progress_towards_break: Duration::milliseconds(12_123),
                progress_towards_reset: Duration::seconds(0),
                idle_state: DebouncedIdleState::Active {
                    active_since: current_time,
                },
            },
            reading_mode: false,
        };
        assert_eq!(idle_monitor.refresh_idle_info(), expected_idle_info);
    }

    #[test]
    fn test_active_starting_transition() {
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
                    muted_until: None,
                    progress_towards_break: Duration::milliseconds(8_000),
                    progress_towards_reset: Duration::seconds(0),
                    idle_state: DebouncedIdleState::Active {
                        active_since: current_time - Duration::milliseconds(1_000),
                    },
                },
                reading_mode: false,
            },
        };
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 1,
            last_checked: current_time,
            last_mode_state: ModeState::Normal {
                muted_until: None,
                progress_towards_break: Duration::milliseconds(9_000),
                progress_towards_reset: Duration::seconds(0),
                idle_state: DebouncedIdleState::ActiveGoingToIdle {
                    active_since: current_time - Duration::milliseconds(1_000),
                    transitioning_since: current_time,
                },
            },
            reading_mode: false,
        };
        assert_eq!(idle_monitor.refresh_idle_info(), expected_idle_info);
    }

    #[test]
    fn test_active_dont_start_transition_to_idle_in_reading_mode() {
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
                    muted_until: None,
                    progress_towards_break: Duration::milliseconds(8_000),
                    progress_towards_reset: Duration::seconds(0),
                    idle_state: DebouncedIdleState::Active {
                        active_since: current_time - Duration::milliseconds(1_000),
                    },
                },
                reading_mode: true,
            },
        };
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 1,
            last_checked: current_time,
            last_mode_state: ModeState::Normal {
                muted_until: None,
                progress_towards_break: Duration::milliseconds(9_000),
                progress_towards_reset: Duration::seconds(0),
                idle_state: DebouncedIdleState::Active {
                    active_since: current_time - Duration::milliseconds(1_000),
                },
            },
            reading_mode: true,
        };
        assert_eq!(idle_monitor.refresh_idle_info(), expected_idle_info);
    }

    #[test]
    fn test_active_still_transitioning() {
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
                    muted_until: None,
                    progress_towards_break: Duration::milliseconds(11_000),
                    progress_towards_reset: Duration::seconds(0),
                    idle_state: DebouncedIdleState::ActiveGoingToIdle {
                        active_since: current_time - Duration::milliseconds(8_000),
                        transitioning_since: current_time - Duration::milliseconds(1_000),
                    },
                },
                reading_mode: false,
            },
        };
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 2,
            last_checked: current_time,
            last_mode_state: ModeState::Normal {
                muted_until: None,
                progress_towards_break: Duration::milliseconds(12_001),
                progress_towards_reset: Duration::seconds(0),
                idle_state: DebouncedIdleState::ActiveGoingToIdle {
                    active_since: current_time - Duration::milliseconds(8_000),
                    transitioning_since: current_time - Duration::milliseconds(1_000),
                },
            },
            reading_mode: false,
        };
        assert_eq!(idle_monitor.refresh_idle_info(), expected_idle_info);
    }

    #[test]
    fn test_active_cancelling_transition() {
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
                    muted_until: None,
                    progress_towards_break: Duration::milliseconds(11_005),
                    progress_towards_reset: Duration::seconds(0),
                    idle_state: DebouncedIdleState::ActiveGoingToIdle {
                        active_since: current_time - Duration::milliseconds(9_000),
                        transitioning_since: current_time - Duration::milliseconds(2_000),
                    },
                },
                reading_mode: false,
            },
        };
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 0,
            last_checked: current_time,
            last_mode_state: ModeState::Normal {
                muted_until: None,
                progress_towards_break: Duration::milliseconds(12_007),
                progress_towards_reset: Duration::seconds(0),
                idle_state: DebouncedIdleState::Active {
                    active_since: current_time - Duration::milliseconds(9_000),
                },
            },
            reading_mode: false,
        };
        assert_eq!(idle_monitor.refresh_idle_info(), expected_idle_info);
    }

    #[test]
    fn test_active_finalizing_transition() {
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
                    muted_until: None,
                    progress_towards_break: Duration::milliseconds(14_020),
                    progress_towards_reset: Duration::seconds(0),
                    idle_state: DebouncedIdleState::ActiveGoingToIdle {
                        active_since: current_time - Duration::milliseconds(9_000),
                        transitioning_since: current_time - Duration::milliseconds(2_000),
                    },
                },
                reading_mode: false,
            },
        };
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 4,
            last_checked: current_time,
            last_mode_state: ModeState::Normal {
                muted_until: None,
                progress_towards_break: Duration::milliseconds(15_025),
                progress_towards_reset: Duration::seconds(0),
                idle_state: DebouncedIdleState::Idle {
                    idle_since: current_time,
                },
            },
            reading_mode: false,
        };
        assert_eq!(idle_monitor.refresh_idle_info(), expected_idle_info);
    }

    #[test]
    fn test_reaches_reset_threshold() {
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
                    muted_until: None,
                    progress_towards_break: Duration::milliseconds(6_000),
                    progress_towards_reset: Duration::milliseconds(
                        BREAK_LENGTH_SECS * 1_000 - 0_050,
                    ),
                    idle_state: DebouncedIdleState::Idle {
                        idle_since: current_time - Duration::milliseconds(5_000),
                    },
                },
                reading_mode: false,
            },
        };
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 1,
            last_checked: current_time,
            last_mode_state: ModeState::Normal {
                muted_until: None,
                progress_towards_break: Duration::seconds(0),
                progress_towards_reset: Duration::seconds(BREAK_LENGTH_SECS),
                idle_state: DebouncedIdleState::Idle {
                    idle_since: current_time - Duration::milliseconds(5_000),
                },
            },
            reading_mode: false,
        };
        assert_eq!(idle_monitor.refresh_idle_info(), expected_idle_info);
    }

    #[test]
    fn test_start_mute_in_normal() {
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
                    muted_until: None,
                    progress_towards_break: Duration::milliseconds(6_000),
                    progress_towards_reset: Duration::milliseconds(2_000),
                    idle_state: DebouncedIdleState::Idle {
                        idle_since: current_time - Duration::milliseconds(5_000),
                    },
                },
                reading_mode: false,
            },
        };
        let resume_at_stamp = current_time + Duration::seconds(5 * 60);
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 0,
            last_checked: current_time - Duration::milliseconds(1_025),
            last_mode_state: ModeState::Normal {
                muted_until: Some(resume_at_stamp),
                progress_towards_break: Duration::milliseconds(6_000),
                progress_towards_reset: Duration::milliseconds(2_000),
                idle_state: DebouncedIdleState::Idle {
                    idle_since: current_time - Duration::milliseconds(5_000),
                },
            },
            reading_mode: false,
        };
        assert_eq!(idle_monitor.mute_until(resume_at_stamp), expected_idle_info);
    }

    #[test]
    fn test_remain_muted() {
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
                    muted_until: Some(current_time + Duration::seconds(1)),
                    progress_towards_break: Duration::milliseconds(0),
                    progress_towards_reset: Duration::milliseconds(5_000),
                    idle_state: DebouncedIdleState::Idle {
                        idle_since: current_time - Duration::milliseconds(5_000),
                    },
                },
                reading_mode: false,
            },
        };
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 6,
            last_checked: current_time,
            last_mode_state: ModeState::Normal {
                muted_until: Some(current_time + Duration::seconds(1)),
                progress_towards_break: Duration::milliseconds(1_025),
                progress_towards_reset: Duration::milliseconds(6_025),
                idle_state: DebouncedIdleState::Idle {
                    idle_since: current_time - Duration::milliseconds(5_000),
                },
            },
            reading_mode: false,
        };
        assert_eq!(idle_monitor.refresh_idle_info(), expected_idle_info);
    }

    #[test]
    fn test_unmute_when_muted() {
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
                    muted_until: Some(current_time + Duration::seconds(1)),
                    progress_towards_break: Duration::milliseconds(0),
                    progress_towards_reset: Duration::milliseconds(5_000),
                    idle_state: DebouncedIdleState::Idle {
                        idle_since: current_time - Duration::milliseconds(5_000),
                    },
                },
                reading_mode: false,
            },
        };
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 5,
            last_checked: current_time - Duration::milliseconds(1_025),
            last_mode_state: ModeState::Normal {
                muted_until: None,
                progress_towards_break: Duration::milliseconds(0),
                progress_towards_reset: Duration::milliseconds(5_000),
                idle_state: DebouncedIdleState::Idle {
                    idle_since: current_time - Duration::milliseconds(5_000),
                },
            },
            reading_mode: false,
        };
        assert_eq!(idle_monitor.unmute(), expected_idle_info);
    }

    #[test]
    fn test_unmute_when_not_muted() {
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
                    muted_until: None,
                    progress_towards_break: Duration::milliseconds(0),
                    progress_towards_reset: Duration::milliseconds(5_000),
                    idle_state: DebouncedIdleState::Idle {
                        idle_since: current_time - Duration::milliseconds(5_000),
                    },
                },
                reading_mode: false,
            },
        };
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 5,
            last_checked: current_time - Duration::milliseconds(1_025),
            last_mode_state: ModeState::Normal {
                muted_until: None,
                progress_towards_break: Duration::milliseconds(0),
                progress_towards_reset: Duration::milliseconds(5_000),
                idle_state: DebouncedIdleState::Idle {
                    idle_since: current_time - Duration::milliseconds(5_000),
                },
            },
            reading_mode: false,
        };
        assert_eq!(idle_monitor.unmute(), expected_idle_info);
    }

    #[test]
    fn test_wake_from_mute() {
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
                    muted_until: Some(current_time - Duration::seconds(1)),
                    progress_towards_break: Duration::milliseconds(0),
                    progress_towards_reset: Duration::milliseconds(5_000),
                    idle_state: DebouncedIdleState::Idle {
                        idle_since: current_time - Duration::milliseconds(5_000),
                    },
                },
                reading_mode: false,
            },
        };
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 6,
            last_checked: current_time,
            last_mode_state: ModeState::Normal {
                muted_until: None,
                progress_towards_break: Duration::milliseconds(1_025),
                progress_towards_reset: Duration::milliseconds(6_025),
                idle_state: DebouncedIdleState::Idle {
                    idle_since: current_time - Duration::milliseconds(5_000),
                },
            },
            reading_mode: false,
        };
        assert_eq!(idle_monitor.refresh_idle_info(), expected_idle_info);
    }

    #[test]
    fn test_reaches_break_threshold() {
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
                    muted_until: None,
                    progress_towards_break: Duration::milliseconds(
                        TIME_TO_BREAK_SECS * 1_000 - 0_089,
                    ),
                    progress_towards_reset: Duration::seconds(0),
                    idle_state: DebouncedIdleState::Active {
                        active_since: current_time - Duration::milliseconds(10_000),
                    },
                },
                reading_mode: false,
            },
        };
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 0,
            last_checked: current_time,
            last_mode_state: ModeState::PreBreak {
                started_at: current_time,
            },
            reading_mode: false,
        };
        assert_eq!(idle_monitor.refresh_idle_info(), expected_idle_info);
    }

    #[test]
    fn test_ignore_break_threshold_in_mute() {
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
                    muted_until: Some(current_time + Duration::seconds(1)),
                    progress_towards_break: Duration::milliseconds(
                        TIME_TO_BREAK_SECS * 1_000 - 0_089,
                    ),
                    progress_towards_reset: Duration::seconds(0),
                    idle_state: DebouncedIdleState::Active {
                        active_since: current_time - Duration::milliseconds(10_000),
                    },
                },
                reading_mode: false,
            },
        };
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 0,
            last_checked: current_time,
            last_mode_state: ModeState::Normal {
                muted_until: Some(current_time + Duration::seconds(1)),
                progress_towards_break: Duration::seconds(TIME_TO_BREAK_SECS),
                progress_towards_reset: Duration::seconds(0),
                idle_state: DebouncedIdleState::Active {
                    active_since: current_time - Duration::milliseconds(10_000),
                },
            },
            reading_mode: false,
        };
        assert_eq!(idle_monitor.refresh_idle_info(), expected_idle_info);
    }

    #[test]
    fn test_prebreak_status_quo() {
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
                reading_mode: false,
            },
        };
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 1,
            last_checked: current_time,
            last_mode_state: ModeState::PreBreak {
                started_at: current_time - Duration::seconds(5),
            },
            reading_mode: false,
        };
        assert_eq!(idle_monitor.refresh_idle_info(), expected_idle_info);
    }

    #[test]
    fn test_prebreak_idle_reset() {
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
                reading_mode: false,
            },
        };
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 0,
            last_checked: current_time,
            last_mode_state: ModeState::PreBreak {
                started_at: current_time - Duration::seconds(5),
            },
            reading_mode: false,
        };
        assert_eq!(idle_monitor.refresh_idle_info(), expected_idle_info);
    }

    #[test]
    fn test_prebreak_idle_requirement_satisfied() {
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
                reading_mode: false,
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
            reading_mode: false,
        };
        assert_eq!(idle_monitor.refresh_idle_info(), expected_idle_info);
    }

    #[test]
    fn test_break_status_quo() {
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
                reading_mode: false,
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
            reading_mode: false,
        };
        assert_eq!(idle_monitor.refresh_idle_info(), expected_idle_info);
    }

    #[test]
    fn test_break_concluded() {
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
                        BREAK_LENGTH_SECS * 1_000 - 0_052,
                    ),
                    idle_state: DebouncedIdleState::Idle {
                        idle_since: current_time - Duration::milliseconds(28_000),
                    },
                },
                reading_mode: false,
            },
        };
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 28,
            last_checked: current_time,
            last_mode_state: ModeState::Normal {
                muted_until: None,
                progress_towards_break: Duration::seconds(0),
                progress_towards_reset: Duration::seconds(BREAK_LENGTH_SECS),
                idle_state: DebouncedIdleState::Idle {
                    idle_since: current_time - Duration::milliseconds(28_000),
                },
            },
            reading_mode: false,
        };
        assert_eq!(idle_monitor.refresh_idle_info(), expected_idle_info);
    }

    #[test]
    fn test_force_break() {
        let current_time = Utc::now();
        let idle_checker = make_idle_checker(0);
        let clock = make_clock(&current_time);

        let mut idle_monitor = IdleMonitor {
            idle_checker,
            clock,
            last_idle_info: IdleInfo {
                idle_since_seconds: 0,
                last_checked: current_time - Duration::milliseconds(1_000),
                last_mode_state: ModeState::Normal {
                    muted_until: None,
                    progress_towards_break: Duration::milliseconds(8_000),
                    progress_towards_reset: Duration::seconds(0),
                    idle_state: DebouncedIdleState::Active {
                        active_since: current_time - Duration::milliseconds(1_000),
                    },
                },
                reading_mode: false,
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
            reading_mode: false,
        };
        assert_eq!(idle_monitor.force_break(), expected_idle_info);
    }

    #[test]
    fn test_start_mute_in_break() {
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
                reading_mode: false,
            },
        };
        let resume_at_stamp = current_time + Duration::seconds(5 * 60);
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 0,
            last_checked: current_time - Duration::milliseconds(1_025),
            last_mode_state: ModeState::Normal {
                muted_until: Some(resume_at_stamp),
                progress_towards_break: Duration::milliseconds(0),
                progress_towards_reset: Duration::milliseconds(0),
                idle_state: DebouncedIdleState::Idle {
                    idle_since: current_time,
                },
            },
            reading_mode: false,
        };
        assert_eq!(idle_monitor.mute_until(resume_at_stamp), expected_idle_info);
    }

    #[test]
    fn test_skip_break() {
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
                reading_mode: false,
            },
        };
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 0,
            last_checked: current_time,
            last_mode_state: ModeState::Normal {
                muted_until: None,
                progress_towards_break: Duration::seconds(0),
                progress_towards_reset: Duration::seconds(0),
                idle_state: DebouncedIdleState::Active {
                    active_since: current_time - Duration::milliseconds(2_000),
                },
            },
            reading_mode: false,
        };
        assert_eq!(idle_monitor.skip_break(), expected_idle_info);
    }

    #[test]
    fn test_postpone_break() {
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
                reading_mode: false,
            },
        };
        let expected_idle_info = IdleInfo {
            idle_since_seconds: 0,
            last_checked: current_time,
            last_mode_state: ModeState::Normal {
                muted_until: None,
                progress_towards_break: Duration::seconds(TIME_TO_BREAK_SECS - (3 * 60)),
                progress_towards_reset: Duration::seconds(0),
                idle_state: DebouncedIdleState::Active {
                    active_since: current_time - Duration::milliseconds(2_000),
                },
            },
            reading_mode: false,
        };
        assert_eq!(
            idle_monitor.postpone_break(Duration::seconds(3 * 60)),
            expected_idle_info
        );
    }
}

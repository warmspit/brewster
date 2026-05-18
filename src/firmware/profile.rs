// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2026 David Bannister

//! Temperature profile runtime state machine.
//!
//! A temperature profile is a named sequence of (target temperature, hold duration)
//! steps.  The controller advances to the next step once the measured temperature
//! has been within the configured deadband for the full hold duration.
//!
//! Flash persistence of profile definitions lives in `storage.rs`.  This module
//! owns only the runtime state (active profile slot, current step, hold timer)
//! plus the public API that the control loop and HTTP task call.

use core::cell::{Cell, RefCell};
use core::sync::atomic::{AtomicBool, AtomicI8, AtomicU8, Ordering};

use critical_section::Mutex;

use super::storage::{self, MAX_STEPS_PER_PROFILE, PROFILE_NAME_MAX_LEN, ProfileStep};

pub use super::storage::ProfileError;

// ── Runtime state ─────────────────────────────────────────────────────────────

/// Active profile slot index; -1 means no profile is running.
static ACTIVE_SLOT: AtomicI8 = AtomicI8::new(-1);
/// Current step index within the active profile.
static ACTIVE_STEP: AtomicU8 = AtomicU8::new(0);
/// Whether the measured temperature is currently within the deadband for the
/// active step's target.  Updated on every `profile_tick` call.
static AT_TARGET: AtomicBool = AtomicBool::new(false);

/// Embassy tick count when the temperature first entered the deadband for the
/// current step.  Zero means we have not reached the target yet this step.
static HOLD_STARTED_TICKS: Mutex<Cell<u64>> = Mutex::new(Cell::new(0));

/// In-RAM copy of the active profile's steps — avoids flash reads in the hot path.
static ACTIVE_STEPS: Mutex<RefCell<heapless::Vec<ProfileStep, MAX_STEPS_PER_PROFILE>>> =
    Mutex::new(RefCell::new(heapless::Vec::new()));

/// In-RAM copy of the active profile's name.
static ACTIVE_NAME: Mutex<RefCell<heapless::String<{ PROFILE_NAME_MAX_LEN }>>> =
    Mutex::new(RefCell::new(heapless::String::new()));

// ── Public types ──────────────────────────────────────────────────────────────

/// Snapshot of the active profile's runtime state, returned for HTTP reporting.
pub struct ActiveProfileState {
    pub name: heapless::String<{ PROFILE_NAME_MAX_LEN }>,
    pub step_index: usize,
    pub total_steps: usize,
    pub step_target_c: f32,
    pub step_hold_secs: u32,
    /// `true` once the temperature has entered the deadband for this step.
    pub at_target: bool,
    /// Seconds elapsed within the current hold (0 if not yet at target).
    pub hold_elapsed_secs: u32,
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Start running a named temperature profile.
///
/// Immediately updates the in-RAM target temperature to the first step's
/// target (without a flash write).  Returns `ProfileError::NotFound` if no
/// profile with that name is stored.
pub fn start_profile(name: &str) -> Result<(), ProfileError> {
    let (slot, profile) = storage::profile_find_by_name(name).ok_or(ProfileError::NotFound)?;
    if profile.steps.is_empty() {
        return Err(ProfileError::InvalidStep);
    }

    let first_target = profile.steps[0].target_c;

    critical_section::with(|cs| {
        *ACTIVE_STEPS.borrow_ref_mut(cs) = profile.steps.clone();
        *ACTIVE_NAME.borrow_ref_mut(cs) = profile.name.clone();
        HOLD_STARTED_TICKS.borrow(cs).set(0);
    });

    ACTIVE_SLOT.store(slot as i8, Ordering::Relaxed);
    ACTIVE_STEP.store(0, Ordering::Relaxed);
    AT_TARGET.store(false, Ordering::Relaxed);

    storage::set_target_temp_c_ram(first_target);
    Ok(())
}

/// Stop the active profile.  The target temperature is left at whatever value
/// was active when the profile stopped.
pub fn stop_profile() {
    ACTIVE_SLOT.store(-1, Ordering::Relaxed);
    ACTIVE_STEP.store(0, Ordering::Relaxed);
    AT_TARGET.store(false, Ordering::Relaxed);
    critical_section::with(|cs| {
        HOLD_STARTED_TICKS.borrow(cs).set(0);
        ACTIVE_STEPS.borrow_ref_mut(cs).clear();
        ACTIVE_NAME.borrow_ref_mut(cs).clear();
    });
}

/// Returns `true` if a temperature profile is currently running.
#[allow(dead_code)]
pub fn is_active() -> bool {
    ACTIVE_SLOT.load(Ordering::Relaxed) >= 0
}

/// Called from the main control loop after each `control_step`.
///
/// Checks whether the measured temperature has been within `deadband_c` of the
/// current step's target for the required hold duration, and advances the
/// profile when it has.
///
/// Returns `Some(new_target_c)` when the profile advanced to a new step.
/// The caller's next `control_step` will detect the setpoint change and reset
/// PID state automatically.  Returns `None` when there is no state change.
pub fn profile_tick(current_temp_c: f32, deadband_c: f32) -> Option<f32> {
    if ACTIVE_SLOT.load(Ordering::Relaxed) < 0 {
        return None;
    }

    let step_idx = ACTIVE_STEP.load(Ordering::Relaxed) as usize;

    // Read the current step from the RAM cache.
    let (step_target_c, step_hold_secs, total_steps) = critical_section::with(|cs| {
        let steps = ACTIVE_STEPS.borrow_ref(cs);
        if step_idx < steps.len() {
            let s = &steps[step_idx];
            (s.target_c, s.hold_secs, steps.len())
        } else {
            (0.0_f32, 0_u32, 0_usize)
        }
    });

    if total_steps == 0 || step_idx >= total_steps {
        stop_profile();
        return None;
    }

    let half_band = deadband_c / 2.0;
    let at_target = (current_temp_c - step_target_c).abs() <= half_band;
    AT_TARGET.store(at_target, Ordering::Relaxed);

    let now_ticks = embassy_time::Instant::now().as_ticks();
    let hold_started = critical_section::with(|cs| HOLD_STARTED_TICKS.borrow(cs).get());

    if !at_target {
        // Drifted outside deadband — reset the hold timer.
        if hold_started != 0 {
            critical_section::with(|cs| HOLD_STARTED_TICKS.borrow(cs).set(0));
        }
        return None;
    }

    if hold_started == 0 {
        // Just arrived at target — start the hold timer.
        critical_section::with(|cs| HOLD_STARTED_TICKS.borrow(cs).set(now_ticks));
        return None;
    }

    // At target with hold timer running — check if the hold has elapsed.
    let elapsed_ticks = now_ticks.saturating_sub(hold_started);
    let required_ticks = step_hold_secs as u64 * embassy_time::TICK_HZ;
    if elapsed_ticks < required_ticks {
        return None;
    }

    // Hold complete — advance to the next step.
    let next_step = step_idx + 1;
    if next_step >= total_steps {
        // Profile finished.
        stop_profile();
        return None;
    }

    let new_target = critical_section::with(|cs| ACTIVE_STEPS.borrow_ref(cs)[next_step].target_c);

    ACTIVE_STEP.store(next_step as u8, Ordering::Relaxed);
    AT_TARGET.store(false, Ordering::Relaxed);
    critical_section::with(|cs| HOLD_STARTED_TICKS.borrow(cs).set(0));

    storage::set_target_temp_c_ram(new_target);
    Some(new_target)
}

/// Returns a snapshot of the active profile's runtime state, or `None` if no
/// profile is currently running.
pub fn active_state() -> Option<ActiveProfileState> {
    if ACTIVE_SLOT.load(Ordering::Relaxed) < 0 {
        return None;
    }

    let step_idx = ACTIVE_STEP.load(Ordering::Relaxed) as usize;
    let at_target = AT_TARGET.load(Ordering::Relaxed);
    let now_ticks = embassy_time::Instant::now().as_ticks();
    let hold_started = critical_section::with(|cs| HOLD_STARTED_TICKS.borrow(cs).get());

    let hold_elapsed_secs = if hold_started > 0 {
        (now_ticks.saturating_sub(hold_started) / embassy_time::TICK_HZ) as u32
    } else {
        0
    };

    critical_section::with(|cs| {
        let steps = ACTIVE_STEPS.borrow_ref(cs);
        let name = ACTIVE_NAME.borrow_ref(cs).clone();
        if step_idx >= steps.len() {
            return None;
        }
        let step = &steps[step_idx];
        Some(ActiveProfileState {
            name,
            step_index: step_idx,
            total_steps: steps.len(),
            step_target_c: step.target_c,
            step_hold_secs: step.hold_secs,
            at_target,
            hold_elapsed_secs,
        })
    })
}

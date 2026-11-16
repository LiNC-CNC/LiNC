//! The limit-switch recovery dialog: entry points, gates, the bounded RECOVER
//! loop and the ASSIST re-arm loop.
//!
//! The numbers and the refusal order come from `linc_core::recovery`; this
//! module only sequences them. One step per command, so a stalled UI leaves the
//! axis parked on a completed increment rather than jogging forever.

use super::*;
use linc_core::command::MachineError;
use linc_core::model::LimitSide;
use linc_core::recovery;

/// How long to wait for a step's motion to settle when the snapshot never
/// reports it active (a simulator, or a move shorter than a poll).
pub(super) const STEP_SETTLE: Duration = Duration::from_millis(1500);
/// A move still reported active after this is a stuck jog: stop it.
pub(super) const STEP_TIMEOUT: Duration = Duration::from_secs(5);
/// ASSIST stops re-arming after this long.
pub(super) const ASSIST_TIMEOUT: Duration = Duration::from_secs(60);
/// Minimum gap between re-arm commands while ASSIST is open.
pub(super) const ARM_INTERVAL: Duration = Duration::from_millis(400);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RecoveryChoice {
    Recover,
    Assist,
    Stop,
    Cancel,
}

impl LincApp {
    /// Open the dialog for the first tripped axis. A tripped axis is the
    /// precondition: an empty recovery dialog would offer two buttons that can
    /// only refuse.
    pub(crate) fn open_limit_recovery(&mut self) {
        if self.safety_gate {
            return;
        }
        if matches!(self.overlay.modal, Some(Modal::LimitRecovery(_))) {
            return;
        }
        if self.overlay.modal.is_some() {
            return;
        }
        let Some((axis, side)) = self.snapshot.tripped_axes().first().cloned() else {
            self.toast("NO LIMIT SWITCH TRIPPED", theme::MUTED);
            return;
        };
        self.overlay.modal = Some(Modal::LimitRecovery(LimitRecovery::new(axis, side)));
    }

    /// Auto-open once per `(axis, side)` and advance the open attempt. Runs in
    /// `sync`, after the command events for this frame have been ingested.
    pub(super) fn tick_limit_recovery(&mut self) {
        let first = self.snapshot.tripped_axes().first().cloned();
        match &first {
            None => self.overlay.limit_prompt_seen = None,
            Some((axis, side)) => {
                if self.overlay.limit_prompt_seen.as_ref() != Some(&(axis.clone(), *side)) {
                    self.overlay.limit_prompt_seen = Some((axis.clone(), *side));
                    if !self.safety_gate
                        && self.overlay.modal.is_none()
                        && !self.snapshot.meta.stale
                    {
                        self.overlay.modal = Some(Modal::LimitRecovery(LimitRecovery::new(
                            axis.clone(),
                            *side,
                        )));
                    }
                }
            }
        }

        let Some(Modal::LimitRecovery(mut dialog)) = self.overlay.modal.clone() else {
            return;
        };
        let now = Instant::now();
        let tripped = self.snapshot.limit_tripped(&dialog.axis);

        match dialog.phase {
            RecoveryPhase::Confirming | RecoveryPhase::Done => {}
            RecoveryPhase::Recovering => {
                if tripped.is_none() {
                    self.finish_recovery(&dialog, now);
                    return;
                }
                if tripped != Some(dialog.side) {
                    dialog.fail("OPPOSITE LIMIT TRIPPED — RECOVERY STOPPED");
                    self.overlay.modal = Some(Modal::LimitRecovery(dialog));
                    return;
                }
                if self.snapshot.task.motion_active {
                    if dialog
                        .sent_at
                        .map(|sent| now.duration_since(sent) > STEP_TIMEOUT)
                        .unwrap_or(false)
                    {
                        self.send_quiet(MachineCommand::AbortProgram);
                        dialog.fail("THE MOVE DID NOT FINISH — RECOVERY STOPPED");
                        self.overlay.modal = Some(Modal::LimitRecovery(dialog));
                    }
                    return;
                }
                // The step's move has settled (or the controller was never
                // observed moving). Re-read the switch before issuing another.
                if dialog
                    .sent_at
                    .map(|sent| now.duration_since(sent) >= STEP_SETTLE)
                    .unwrap_or(true)
                {
                    if dialog.issued >= recovery::MAX_STEPS {
                        dialog.fail(format!(
                            "STILL TRIPPED AFTER {} STEPS — SWITCH MAY BE FAULTY, MOTION LEFT DISABLED",
                            dialog.issued
                        ));
                    } else if let Err(error) = self.backend.send(MachineCommand::RecoverLimit {
                        axis: dialog.axis.clone(),
                    }) {
                        dialog.fail(error.to_string().to_uppercase());
                    } else {
                        dialog.issued += 1;
                        dialog.sent_at = Some(now);
                        dialog.message =
                            Some(format!("STEP {} OF {}", dialog.issued, recovery::MAX_STEPS));
                    }
                }
                self.overlay.modal = Some(Modal::LimitRecovery(dialog));
            }
            RecoveryPhase::Assisting => {
                if tripped.is_none() {
                    self.finish_recovery(&dialog, now);
                    return;
                }
                if tripped != Some(dialog.side) {
                    dialog.fail("OPPOSITE LIMIT TRIPPED — RECOVERY STOPPED");
                    self.overlay.modal = Some(Modal::LimitRecovery(dialog));
                    return;
                }
                if !matches!(
                    self.snapshot.panel.mode.mode(),
                    Some(PanelMode::Jog | PanelMode::Inc | PanelMode::Mpg)
                ) {
                    dialog.fail("SELECT JOG ON THE PANEL");
                    self.overlay.modal = Some(Modal::LimitRecovery(dialog));
                    return;
                }
                if dialog
                    .assist_until
                    .map(|until| now >= until)
                    .unwrap_or(true)
                {
                    dialog.fail("OVERRIDE TIMEOUT — RECOVERY STOPPED");
                    self.overlay.modal = Some(Modal::LimitRecovery(dialog));
                    return;
                }
                // LinuxCNC clears the mask when the move ends, so ASSIST arms
                // again after each jog. Re-read the mask first: a second trip
                // must not be masked by a blind re-arm.
                let armed = self
                    .snapshot
                    .machine
                    .overridden
                    .get(&dialog.axis)
                    .copied()
                    .unwrap_or(false);
                if !armed
                    && dialog
                        .last_arm
                        .map(|last| now.duration_since(last) >= ARM_INTERVAL)
                        .unwrap_or(true)
                {
                    if let Err(error) = self.backend.send(MachineCommand::ArmLimitsOverride {
                        axis: dialog.axis.clone(),
                    }) {
                        dialog.fail(error.to_string().to_uppercase());
                    } else {
                        dialog.last_arm = Some(now);
                    }
                }
                self.overlay.modal = Some(Modal::LimitRecovery(dialog));
            }
        }
    }

    /// One RECOVER/ASSIST/STOP/CANCEL press.
    pub(super) fn limit_recovery_action(&mut self, choice: RecoveryChoice) {
        let Some(Modal::LimitRecovery(mut dialog)) = self.overlay.modal.clone() else {
            return;
        };
        let now = Instant::now();
        match choice {
            RecoveryChoice::Cancel => self.overlay.modal = None,
            RecoveryChoice::Stop => {
                self.send_quiet(MachineCommand::AbortProgram);
                dialog.fail("STOPPED BY THE OPERATOR");
                self.overlay.modal = Some(Modal::LimitRecovery(dialog));
            }
            RecoveryChoice::Recover | RecoveryChoice::Assist => {
                if let Err(error) = recovery::check_gates(&self.snapshot, &dialog.axis) {
                    dialog.fail(recovery_error_text(&error));
                    self.overlay.modal = Some(Modal::LimitRecovery(dialog));
                    return;
                }
                if choice == RecoveryChoice::Recover {
                    match self.backend.send(MachineCommand::RecoverLimit {
                        axis: dialog.axis.clone(),
                    }) {
                        Ok(()) => {
                            dialog.phase = RecoveryPhase::Recovering;
                            dialog.issued = 1;
                            dialog.sent_at = Some(now);
                            dialog.message = Some(format!("STEP 1 OF {}", recovery::MAX_STEPS));
                        }
                        Err(error) => dialog.fail(error.to_string().to_uppercase()),
                    }
                } else {
                    match self.backend.send(MachineCommand::ArmLimitsOverride {
                        axis: dialog.axis.clone(),
                    }) {
                        Ok(()) => {
                            dialog.phase = RecoveryPhase::Assisting;
                            dialog.assist_until = Some(now + ASSIST_TIMEOUT);
                            dialog.last_arm = Some(now);
                            dialog.message = Some("OVERRIDE ARMED".into());
                        }
                        Err(error) => dialog.fail(error.to_string().to_uppercase()),
                    }
                }
                self.overlay.modal = Some(Modal::LimitRecovery(dialog));
            }
        }
    }

    fn finish_recovery(&mut self, dialog: &LimitRecovery, _now: Instant) {
        self.overlay.modal = None;
        // ASSIST issues no steps of its own: the operator's jog cleared it.
        let text = if dialog.phase == RecoveryPhase::Assisting {
            format!("{} CLEARED", dialog.axis)
        } else {
            let steps = dialog.issued.max(1);
            let noun = if steps == 1 { "STEP" } else { "STEPS" };
            format!("{} CLEARED AFTER {steps} {noun}", dialog.axis)
        };
        self.toast(text, theme::SUCCESS);
    }

    /// The reason RECOVER/ASSIST are disabled, or `None` when they may run.
    pub(super) fn limit_recovery_block(&self, axis: &str) -> Option<String> {
        recovery::check_gates(&self.snapshot, axis)
            .err()
            .map(|error| recovery_error_text(&error))
    }

    /// Direction the operator must hold during ASSIST.
    fn assist_direction(&self, dialog: &LimitRecovery) -> String {
        match dialog.side {
            LimitSide::Min => format!("{}+", dialog.axis),
            LimitSide::Max => format!("{}-", dialog.axis),
        }
    }

    /// Draw the dialog body and return the pressed action, if any.
    pub(super) fn draw_limit_recovery(
        &self,
        ui: &mut egui::Ui,
        first_frame: bool,
        dialog: &LimitRecovery,
    ) -> Option<RecoveryChoice> {
        let mut choice = None;
        let limit = recovery::hard_limit_label(&dialog.axis, dialog.side);
        let away = recovery::away_label(dialog.side);
        ui.label(
            RichText::new("LIMIT SWITCH TRIPPED")
                .size(theme::font_size(18.0))
                .strong()
                .color(theme::ERROR),
        );
        ui.add_space(6.0);
        let motion = if self.snapshot.machine.on {
            "MOTION IS ENABLED."
        } else {
            "MOTION IS DISABLED AND HOME WILL NOT WORK UNTIL IT IS RE-ENABLED."
        };
        ui.label(format!("{limit} TRIPPED. {motion}"));
        ui.add_space(4.0);
        ui.label(format!("{} CAN ONLY MOVE {away} FROM HERE.", dialog.axis));
        ui.add_space(10.0);

        match dialog.phase {
            RecoveryPhase::Confirming => {
                let block = self.limit_recovery_block(&dialog.axis);
                ui.horizontal(|ui| {
                    if theme::accent_button(ui, "RECOVER", block.is_none()).clicked() {
                        choice = Some(RecoveryChoice::Recover);
                    }
                    if theme::action_button(ui, "ASSIST", block.is_none()).clicked() {
                        choice = Some(RecoveryChoice::Assist);
                    }
                    let cancel = theme::action_button(ui, "CANCEL", true);
                    if first_frame {
                        cancel.request_focus();
                    }
                    if cancel.clicked() {
                        choice = Some(RecoveryChoice::Cancel);
                    }
                });
                ui.add_space(6.0);
                ui.label(
                    RichText::new(format!(
                        "RECOVER   LIMITS OVERRIDE, ONE BOUNDED JOG OF {:.1} mm AT {:.1} mm/s, REPEATED UP TO {} STEPS ({} mm).",
                        recovery::STEP_MM,
                        recovery::FEED_MM_S,
                        recovery::MAX_STEPS,
                        recovery::STEP_MM * f64::from(recovery::MAX_STEPS)
                    ))
                    .size(theme::font_size(10.0))
                    .color(theme::MUTED),
                );
                ui.label(
                    RichText::new(
                        "ASSIST    ARM THE OVERRIDE, THEN YOU HOLD THE JOG KEY ON THE PANEL.",
                    )
                    .size(theme::font_size(10.0))
                    .color(theme::MUTED),
                );
                if let Some(reason) = block {
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(reason)
                            .size(theme::font_size(10.0))
                            .color(theme::WARNING),
                    );
                }
            }
            RecoveryPhase::Recovering | RecoveryPhase::Assisting | RecoveryPhase::Done => {
                ui.horizontal(|ui| {
                    match dialog.phase {
                        RecoveryPhase::Recovering | RecoveryPhase::Assisting
                            if theme::danger_button(ui, "STOP", true).clicked() =>
                        {
                            choice = Some(RecoveryChoice::Stop);
                        }
                        _ => {}
                    }
                    let cancel = theme::action_button(ui, "CANCEL", true);
                    if first_frame {
                        cancel.request_focus();
                    }
                    if cancel.clicked() {
                        choice = Some(RecoveryChoice::Cancel);
                    }
                });
                ui.add_space(6.0);
                if dialog.phase == RecoveryPhase::Assisting {
                    let armed = self
                        .snapshot
                        .machine
                        .overridden
                        .get(&dialog.axis)
                        .copied()
                        .unwrap_or(false);
                    let text = if armed {
                        format!(
                            "OVERRIDE ARMED — HOLD {} ON THE PANEL. RELEASE WHEN THE SWITCH CLEARS.",
                            self.assist_direction(dialog)
                        )
                    } else {
                        format!(
                            "RE-ARMING THE OVERRIDE — HOLD {} ON THE PANEL.",
                            self.assist_direction(dialog)
                        )
                    };
                    ui.label(
                        RichText::new(text)
                            .size(theme::font_size(12.0))
                            .strong()
                            .color(if armed { theme::WARNING } else { theme::MUTED }),
                    );
                    ui.label(
                        RichText::new(format!("{} TRIPPED", limit))
                            .size(theme::font_size(10.0))
                            .color(theme::ERROR),
                    );
                }
                if let Some(message) = &dialog.message {
                    ui.label(RichText::new(message).size(theme::font_size(11.0)).color(
                        if dialog.phase == RecoveryPhase::Done {
                            theme::ERROR
                        } else {
                            theme::MUTED
                        },
                    ));
                }
            }
        }

        ui.add_space(10.0);
        ui.label(
            RichText::new(
                "DO NOT PRESS POWER OFF: ON THIS MACHINE THAT ENDS THE SESSION AND POWERS OFF THE PC.",
            )
            .size(theme::font_size(10.0))
            .color(theme::WARNING),
        );
        choice
    }
}

/// A gate's refusal in the operator's words. The generic `WrongPanelMode` text
/// says what is wrong but not what to do; recovery needs JOG/INC/MPG named.
fn recovery_error_text(error: &MachineError) -> String {
    match error {
        MachineError::WrongPanelMode => "SELECT JOG ON THE PANEL (JOG, INC OR MPG)".into(),
        other => other.to_string().to_ascii_uppercase(),
    }
}

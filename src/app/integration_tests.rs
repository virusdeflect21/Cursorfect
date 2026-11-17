use super::*;

/// Runs the actual engine routing/cancellation methods. No HookThread is
/// installed, no real cursor movement/shape or user settings are changed.
#[test]
fn mih_engine_routes_modes_conflicts_recording_master_emergency_and_exit() {
    let mut settings = Settings {
        master_enabled: true,
        effect: EffectKind::MadeInHeaven,
        keep_arrow_cursor: false,
        ..Settings::default()
    };
    settings.audio.enabled = false;
    settings.made_in_heaven.ramp_up_secs = 5.0;
    settings.made_in_heaven.duration_secs = 3.0;
    let original_activation = settings.activation;
    let original_dash = settings.dash;
    let (tx, rx) = std::sync::mpsc::channel();
    let hooks = HookShared::new(
        tx.clone(),
        Bindings {
            activation: settings.active_shortcut(),
            dash: settings.dash,
            consume_mouse: true,
            dash_enabled: false,
            release_on_modifier_up: false,
        },
    );
    let mut engine = Engine::new(EngineArgs {
        settings: settings.clone(),
        rx,
        tx,
        hooks: hooks.clone(),
        ui: Box::new(|_| {}),
    });
    let start = Instant::now();
    let at = |seconds: f32| start + Duration::from_secs_f32(seconds);
    for (index, mode) in [
        TimeStopMode::Toggle,
        TimeStopMode::Hold,
        TimeStopMode::Timed,
    ]
    .into_iter()
    .enumerate()
    {
        let base = index as f32 * 10.0;
        settings.made_in_heaven.mode = mode;
        engine.apply_settings(settings.clone(), at(base), false);
        engine.on_input(InputEvent::ActivationDown, at(base));
        engine.update_mih(at(base + 2.0));
        assert!(engine.mih.is_busy());
        assert!(!engine.machine.is_active());
        assert!(engine.tw.is_idle());
        assert!(hooks.acceleration.test_multiplier(at(base + 2.0)) > 1.0);
        if mode == TimeStopMode::Hold {
            engine.on_input(InputEvent::ActivationUp, at(base + 2.0));
        } else if mode == TimeStopMode::Toggle {
            engine.on_input(InputEvent::ActivationDown, at(base + 2.0));
        }
        engine.update_mih(at(base + 4.6));
        assert!(!engine.mih.is_busy());
        assert_eq!(hooks.acceleration.test_multiplier(at(base + 4.6)), 1.0);
    }
    settings.made_in_heaven.mode = TimeStopMode::Toggle;
    settings.made_in_heaven.repeat_after_reset = true;
    engine.apply_settings(settings.clone(), at(30.0), false);
    engine.on_input(InputEvent::ActivationDown, at(30.0));
    engine.update_mih(at(35.0));
    assert_eq!(engine.mih.snapshot(at(35.0)).phase, MihPhase::UniverseReset);
    engine.update_mih(at(36.5));
    assert!(engine.mih.is_busy());
    assert_eq!(engine.mih.snapshot(at(36.5)).progress, 0.0);
    settings.master_enabled = false;
    engine.apply_settings(settings.clone(), at(37.0), false);
    assert!(!engine.mih.is_busy());
    assert_eq!(hooks.acceleration.test_multiplier(at(37.0)), 1.0);
    // Real acceleration can be turned off while visuals keep running.
    settings.master_enabled = true;
    settings.made_in_heaven.repeat_after_reset = false;
    engine.apply_settings(settings.clone(), at(40.0), false);
    engine.on_input(InputEvent::ActivationDown, at(40.0));
    engine.update_mih(at(41.0));
    settings.made_in_heaven.real_acceleration = false;
    engine.apply_settings(settings.clone(), at(41.0), false);
    engine.update_mih(at(42.0));
    assert!(engine.mih.is_busy());
    assert!(engine.mih.snapshot(at(42.0)).multiplier > 1.0);
    assert_eq!(hooks.acceleration.test_multiplier(at(42.0)), 1.0);
    // Effect switches use the real manager and never leave two input owners active.
    settings.effect = EffectKind::Sandevistan;
    engine.apply_settings(settings.clone(), at(43.0), false);
    assert!(!engine.mih.is_busy());
    engine.on_input(InputEvent::ActivationDown, at(43.0));
    assert!(engine.machine.is_active());
    assert!(engine.effects.is_active());
    settings.effect = EffectKind::MadeInHeaven;
    engine.apply_settings(settings.clone(), at(44.0), false);
    assert!(!engine.machine.is_active());
    engine.on_input(InputEvent::ActivationDown, at(44.0));
    assert!(engine.mih.is_busy());
    assert!(!engine.effects.is_active());
    settings.effect = EffectKind::TheWorld;
    engine.apply_settings(settings.clone(), at(45.0), false);
    engine.tw.begin(
        at(45.0),
        Vec2::ZERO,
        1.0,
        TwOptions::default(),
        WinRect {
            l: 0.0,
            t: 0.0,
            r: 1920.0,
            b: 1080.0,
        },
    );
    settings.effect = EffectKind::MadeInHeaven;
    engine.apply_settings(settings.clone(), at(45.1), false);
    engine.on_input(InputEvent::ActivationDown, at(45.1));
    assert!(engine.tw.is_idle());
    // Recording ends MIH before changing its release binding; old shortcuts survive.
    engine.on_cmd(Cmd::Record(RECORD_ACTIVATION), at(46.0));
    assert!(!engine.mih.is_busy());
    let shortcut = Shortcut::key(119, crate::input::shortcuts::Modifiers::NONE);
    engine.on_recorded(shortcut);
    assert_eq!(engine.settings.made_in_heaven.activation, shortcut);
    assert_eq!(engine.settings.activation, original_activation);
    assert_eq!(engine.settings.dash, original_dash);
    engine.on_input(InputEvent::ActivationDown, at(47.0));
    engine.emergency(at(48.0));
    assert!(!engine.mih.is_busy());
    assert!(!hooks.master.load(Ordering::Acquire));
    assert_eq!(hooks.acceleration.test_multiplier(at(48.0)), 1.0);
    // Drop/unwind cleanup also restores input, without invoking save_now in this test.
    settings.master_enabled = true;
    settings.made_in_heaven.real_acceleration = true;
    engine.apply_settings(settings, at(50.0), false);
    engine.on_input(InputEvent::ActivationDown, at(50.0));
    engine.update_mih(at(52.0));
    assert!(hooks.acceleration.test_multiplier(at(52.0)) > 1.0);
    drop(engine);
    assert_eq!(hooks.acceleration.test_multiplier(at(52.0)), 1.0);
    assert!(!hooks.time_stopped.load(Ordering::Acquire));
}

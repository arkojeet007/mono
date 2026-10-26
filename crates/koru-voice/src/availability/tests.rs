// SPDX-License-Identifier: MPL-2.0

use super::*;

/// One test, holding the environment for the whole of it: two tests setting
/// and clearing one process-global name would race under cargo's parallel
/// runner, which is the `THEME_LOCK` lesson without needing a lock.
#[test]
fn the_kill_switch_is_read_by_the_one_rule() {
    let restore = std::env::var(DISABLE_VAR).ok();
    let set = |value: Option<&str>| match value {
        // Single-threaded within this test; no other test in this crate reads
        // or writes this name.
        Some(value) => unsafe { std::env::set_var(DISABLE_VAR, value) },
        None => unsafe { std::env::remove_var(DISABLE_VAR) },
    };

    set(None);
    assert!(!disabled(), "unset means the switch is off");
    // Offered, except on a platform this build has no voice runtime for.
    assert_eq!(available(), !PLATFORM_OFF);
    assert_eq!(off(), reason(false, PLATFORM_OFF));

    set(Some("1"));
    assert!(disabled());
    assert!(!available());
    assert_eq!(off(), reason(true, PLATFORM_OFF));

    // The rule that used to be four rules: `=0` means off, so it means voice
    // is *on*. A presence-only reading here would disable dictation for
    // anybody who wrote `set KORU_VOICE_DISABLED=0` to turn it back on.
    for value in ["", "0", "false", "No", " OFF "] {
        set(Some(value));
        assert!(!disabled(), "value {value:?} should leave voice available");
        assert_eq!(off(), reason(false, PLATFORM_OFF));
    }
    for on in ["1", "true", "yes", "anything"] {
        set(Some(on));
        assert!(disabled(), "value {on:?} should disable voice");
        assert_eq!(off(), reason(true, PLATFORM_OFF));
    }

    set(restore.as_deref());
}

/// The name is the registry's, not a literal — the whole reason `koru-env`
/// exists. A typo here is a switch nobody can find in `koru.bat envs` or
/// `reference/ENV.md`.
#[test]
fn the_name_comes_from_the_registry() {
    assert_eq!(DISABLE_VAR, "KORU_VOICE_DISABLED");
    assert!(
        koru_env::get(DISABLE_VAR).is_some(),
        "the switch is not declared in koru-env's VARS"
    );
}

/// **Which reason wins, without the environment.** Pure arithmetic over both
/// inputs, so it runs on every platform beside the one test that owns the
/// variable, and races nothing.
#[test]
fn the_reason_is_chosen_by_one_rule() {
    assert_eq!(reason(false, false), None);
    assert_eq!(reason(true, false), Some(Off::Switch));
    // The platform names the remedy that is true: unsetting the switch would
    // not turn dictation on.
    assert_eq!(reason(false, true), Some(Off::Platform));
    assert_eq!(reason(true, true), Some(Off::Platform));
}

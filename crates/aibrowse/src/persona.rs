//! The profile's persona and the real user's kinematics (docs/design.md §5.9):
//! whether handoffs record them (the profile's policy), and `aibrowse persona`, which
//! shows the recording and fits the persona to it.
//!
//!   aibrowse persona show [--profile NAME]
//!   aibrowse persona fit [--profile NAME] [--dry-run]

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use aibrowse_handoff::{Recording, load_kinematics};
use aibrowse_input::Persona;
use aibrowse_input::kinematics::{Fit, Kinematics, fit};
use anyhow::{Context, bail};

const USAGE: &str = "usage: aibrowse persona show|fit [--profile NAME] [--dry-run]";

/// What the profile's `policy.json` says about recording, under `"kinematics"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    /// The default: nothing is recorded.
    Off,
    /// Handoffs record statistics; `aibrowse persona fit` applies them.
    Record,
    /// Handoffs record, and the persona is refitted after each one.
    Fit,
}

impl Policy {
    pub fn of(profile_root: &Path) -> Self {
        let policy = std::fs::read(profile_root.join("policy.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
        match policy.as_ref().and_then(|p| p["kinematics"].as_str()) {
            Some("record") => Self::Record,
            Some("fit") => Self::Fit,
            _ => Self::Off,
        }
    }
}

/// Where handoffs should record for this profile, if its policy opts in.
pub fn recording(profile_root: &Path) -> Option<Recording> {
    let policy = Policy::of(profile_root);
    if policy == Policy::Off {
        return None;
    }
    let after_save = (policy == Policy::Fit).then(|| {
        let root = profile_root.to_owned();
        // The running browser keeps the persona it launched with: a persona that shifts
        // mid-session is the inconsistency the persona exists to avoid. The refit
        // applies from the next launch.
        Arc::new(move || match fit_profile(&root, true) {
            Ok(fit) if fit.changed() => tracing::info!(
                profile = %root.display(),
                "persona refitted to the recorded kinematics"
            ),
            Ok(_) => {}
            Err(err) => tracing::warn!(%err, "could not refit the persona"),
        }) as Arc<dyn Fn() + Send + Sync>
    });
    Some(Recording {
        file: kinematics_file(profile_root),
        after_save,
    })
}

fn kinematics_file(profile_root: &Path) -> PathBuf {
    profile_root.join("kinematics.json")
}

/// Fits the profile's persona to its recorded kinematics, saving it when `apply`.
pub fn fit_profile(profile_root: &Path, apply: bool) -> anyhow::Result<Fit> {
    let persona_file = profile_root.join("persona.json");
    let persona: Persona = serde_json::from_slice(
        &std::fs::read(&persona_file)
            .with_context(|| format!("reading {}", persona_file.display()))?,
    )
    .with_context(|| format!("parsing {}", persona_file.display()))?;
    let kinematics = load_kinematics(&kinematics_file(profile_root))
        .context("reading the recorded kinematics")?;
    let fitted = fit(&persona, &kinematics);
    if apply && fitted.changed() {
        let partial = persona_file.with_extension("json.partial");
        std::fs::write(&partial, serde_json::to_vec_pretty(&fitted.persona)?)?;
        std::fs::rename(&partial, &persona_file)?;
    }
    Ok(fitted)
}

pub fn run(args: &[String]) -> anyhow::Result<()> {
    let mut profile = std::env::var("AIBROWSE_PROFILE").unwrap_or_else(|_| "default".to_owned());
    let mut dry_run = false;
    let command = args.first().map(String::as_str);
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--profile" => profile = iter.next().context("--profile needs a value")?.clone(),
            "--dry-run" => dry_run = true,
            other => bail!("unknown option {other:?}\n{USAGE}"),
        }
    }
    let root = dirs::home_dir()
        .context("no home directory")?
        .join(".aibrowse/profiles")
        .join(&profile);
    if !root.join("persona.json").exists() {
        bail!("profile {profile:?} has no persona yet (it is created on first launch)");
    }
    let kinematics = load_kinematics(&kinematics_file(&root))?;
    match command {
        Some("show") => print!("{}", describe_recording(&profile, &root, &kinematics)),
        Some("fit") => {
            let fitted = fit_profile(&root, !dry_run)?;
            print!("{}", describe_recording(&profile, &root, &kinematics));
            print!("{}", describe_fit(&fitted));
            println!(
                "{}",
                match (fitted.changed(), dry_run) {
                    (false, _) => "Nothing changed: not enough samples yet.",
                    (true, true) => "Dry run: persona.json left as it was.",
                    (true, false) => "Saved persona.json; it applies from the next browser launch.",
                }
            );
        }
        _ => bail!("{USAGE}"),
    }
    Ok(())
}

fn describe_recording(profile: &str, root: &Path, kinematics: &Kinematics) -> String {
    let mut out = format!(
        "Profile {profile:?}: recording is {:?}; {} handoffs recorded.\n",
        Policy::of(root),
        kinematics.handoffs
    );
    for line in kinematics.summary() {
        let _ = writeln!(out, "  {line}");
    }
    out
}

/// The before/after table.
pub fn describe_fit(fitted: &Fit) -> String {
    let mut out = String::from("Persona fit (before → after; observed; samples/needed):\n");
    let (before, after) = fitted.device;
    let _ = writeln!(
        out,
        "  {:<22} {before:?} → {after:?} ({} trackpad / {} notched gestures)",
        "device", fitted.gestures.0, fitted.gestures.1
    );
    for p in &fitted.params {
        let observed = p
            .observed
            .map_or_else(|| "–".to_owned(), |v| format!("{v:.3}"));
        let mark = if p.changed() { "*" } else { " " };
        let _ = writeln!(
            out,
            "{mark} {:<22} {:>8.3} → {:>8.3}; observed {observed}; {}/{}",
            p.name, p.before, p.after, p.samples, p.needed
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recording_is_off_unless_the_policy_opts_in() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(Policy::of(dir.path()), Policy::Off);
        assert!(recording(dir.path()).is_none());
        std::fs::write(dir.path().join("policy.json"), r#"{"confirm":"off"}"#).unwrap();
        assert_eq!(Policy::of(dir.path()), Policy::Off);
        std::fs::write(dir.path().join("policy.json"), r#"{"kinematics":"record"}"#).unwrap();
        let record = recording(dir.path()).unwrap();
        assert!(record.after_save.is_none());
        assert_eq!(record.file, dir.path().join("kinematics.json"));
        std::fs::write(dir.path().join("policy.json"), r#"{"kinematics":"fit"}"#).unwrap();
        assert!(recording(dir.path()).unwrap().after_save.is_some());
    }

    #[test]
    fn fitting_a_profile_saves_only_when_asked_and_something_changed() {
        let dir = tempfile::tempdir().unwrap();
        let persona_file = dir.path().join("persona.json");
        let json = serde_json::to_vec(&Persona::from_seed(77)).unwrap();
        std::fs::write(&persona_file, &json).unwrap();
        // As stored: JSON may round the last bit of a float.
        let persona: Persona = serde_json::from_slice(&json).unwrap();
        // No recording yet: nothing to fit.
        assert!(!fit_profile(dir.path(), true).unwrap().changed());

        let mut kinematics = Kinematics::default();
        // Enough samples that the data outweighs the seeded prior (weight n / (n + 50)).
        for _ in 0..1000 {
            kinematics.keys.hold_ms.add(140.0);
        }
        std::fs::write(
            dir.path().join("kinematics.json"),
            serde_json::to_vec(&kinematics).unwrap(),
        )
        .unwrap();
        let dry = fit_profile(dir.path(), false).unwrap();
        assert!(dry.changed());
        let saved: Persona =
            serde_json::from_slice(&std::fs::read(&persona_file).unwrap()).unwrap();
        assert_eq!(saved, persona);

        let applied = fit_profile(dir.path(), true).unwrap();
        let saved: Persona =
            serde_json::from_slice(&std::fs::read(&persona_file).unwrap()).unwrap();
        assert_eq!(saved, applied.persona);
        assert!((saved.key_hold_median_ms - 140.0).abs() < 10.0);
        assert!(describe_fit(&applied).contains("* key_hold_median_ms"));
    }
}

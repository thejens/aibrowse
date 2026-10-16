//! Goals made of several parts, done in order: "enter =A2+10 in C3", then "select C3 to
//! C10", then "fill the formula down". The agent names the parts (it writes; Jev only
//! decides), and each is pursued from where the last one left the page, with its own
//! check that it is done. A part that isn't reached stops the rest: later parts build on
//! earlier ones, and doing them anyway would act on the wrong selection or page.

use crate::jev::{Jev, JevError};
use crate::pursue::{Limits, Outcome, PartOf, Report, Workspace, pursue};

/// Pursues `goals` in order with the same texts and keys, until one isn't reached.
/// Returns each part pursued with its report; parts after a stop are left out.
pub async fn pursue_parts<W: Workspace>(
    workspace: &mut W,
    jev: &Jev,
    goals: &[String],
    texts: &[String],
    limits: &Limits,
) -> Result<Vec<(String, Report)>, JevError> {
    let mut done: Vec<(String, Report)> = Vec::new();
    for (n, goal) in goals.iter().enumerate() {
        let mut part_limits = limits.clone();
        part_limits.part_of = Some(PartOf {
            before: goals[..n].to_vec(),
            after: goals[n + 1..].to_vec(),
            came_from: done.last().and_then(|(_, report)| report.came_from.clone()),
        });
        // Only the first part may need finding; the others act on the page it reached,
        // where looking the part up across the site would only leave it.
        if n > 0 {
            part_limits.lookup = false;
        }
        let report = pursue(workspace, jev, goal, texts, &part_limits).await?;
        let reached = matches!(report.outcome, Outcome::Reached);
        done.push((goal.clone(), report));
        if !reached {
            break;
        }
    }
    Ok(done)
}

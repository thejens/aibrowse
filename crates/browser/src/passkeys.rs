//! Passkey requests when headless. Chrome answers `navigator.credentials` through its
//! own dialog, and headless Chrome has no window to show one in: the request never
//! settles. A sign-in page that asks for a passkey (Google's does, unprompted) then waits
//! forever, and its "Try another way" stays dead while it does, even for a user in the
//! live view. A stand-in authenticator that holds no credentials and fails user
//! verification turns every such request into the NotAllowedError a person declining
//! the dialog would cause, so the page falls back to a password or another method.

use aibrowse_cdp::Session;
use serde_json::json;

use crate::error::Result;

/// Routes the tab's WebAuthn requests to an authenticator that declines them.
pub(crate) async fn decline(session: &Session) -> Result<()> {
    // No UI: there is no window to show it in.
    session
        .call("WebAuthn.enable", json!({ "enableUI": false }))
        .await?;
    session
        .call(
            "WebAuthn.addVirtualAuthenticator",
            json!({ "options": {
                "protocol": "ctap2",
                "ctap2Version": "ctap2_1",
                "transport": "internal",
                "hasResidentKey": true,
                "hasUserVerification": true,
                // Load-bearing: verification fails, so it can't register a passkey either
                // (a site would keep a credential that vanishes with this authenticator).
                "isUserVerified": false,
                // Presence is answered at once; otherwise requests would hang as before.
                "automaticPresenceSimulation": true,
            } }),
        )
        .await?;
    Ok(())
}

//! Form tests: field types, submit buttons, purposes and signatures.

use super::*;

fn facts(tag: &str, attrs: &[(&str, &str)]) -> ElementFacts {
    ElementFacts {
        tag: tag.into(),
        attributes: attrs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect(),
        ..ElementFacts::default()
    }
}

#[test]
fn types_fields_by_what_they_ask_for() {
    let kind =
        |role: &str, name: &str, f: ElementFacts| field(role, name, Some(&f)).map(|f| f.kind);
    assert_eq!(
        kind(
            "textbox",
            "Login",
            facts("input", &[("autocomplete", "username")])
        ),
        Some("username")
    );
    assert_eq!(
        kind(
            "textbox",
            "Password",
            facts("input", &[("type", "password")])
        ),
        Some("password")
    );
    assert_eq!(
        kind(
            "textbox",
            "Code",
            facts(
                "input",
                &[("autocomplete", "one-time-code"), ("type", "text")]
            )
        ),
        Some("otp")
    );
    assert_eq!(
        kind("textbox", "Verification code", facts("input", &[])),
        Some("otp")
    );
    assert_eq!(
        kind("textbox", "Your e-mail", facts("input", &[])),
        Some("email")
    );
    assert_eq!(
        kind("combobox", "Country", facts("select", &[])),
        Some("select")
    );
    assert_eq!(
        kind("textbox", "Message", facts("textarea", &[])),
        Some("textarea")
    );
    // A checkbox stays a checkbox whatever its autocomplete says.
    assert_eq!(
        kind(
            "checkbox",
            "Remember me",
            facts("input", &[("autocomplete", "off")])
        ),
        Some("checkbox")
    );
    assert_eq!(kind("button", "Go", facts("button", &[])), None);
}

#[test]
fn native_buttons_submit_by_default() {
    assert!(is_submit("button", Some(&facts("button", &[]))));
    assert!(is_submit(
        "button",
        Some(&facts("input", &[("type", "submit")]))
    ));
    assert!(!is_submit(
        "button",
        Some(&facts("button", &[("type", "button")]))
    ));
    assert!(!is_submit("button", Some(&facts("div", &[]))));
}

#[test]
fn submit_like_names() {
    for name in ["Sign in", "Save changes", "Subscribe", "OK", "Add to cart"] {
        assert!(is_submit_like(name), "{name}");
    }
    for name in [
        "Cancel",
        "Signal",
        "Gopher",
        "Forgot password?",
        "Close",
        "",
    ] {
        assert!(!is_submit_like(name), "{name}");
    }
}

#[test]
fn purposes() {
    assert_eq!(purpose(&["username", "password"]), Some("login"));
    assert_eq!(
        purpose(&["email", "new-password", "checkbox"]),
        Some("signup")
    );
    assert_eq!(purpose(&["password", "password"]), Some("signup"));
    assert_eq!(purpose(&["otp"]), Some("verification"));
    assert_eq!(purpose(&["search"]), Some("search"));
    assert_eq!(purpose(&["text", "textarea"]), None);
}

fn node(kind: NodeKind, depth: usize, role: &str, name: &str, group: Option<&str>) -> Node {
    Node {
        kind,
        depth,
        role: role.into(),
        name: name.into(),
        reference: None,
        value: None,
        description: None,
        states: Vec::new(),
        effects: Vec::new(),
        href: None,
        position: None,
        distance: 0.0,
        rect: None,
        form_part: group.map(|g| {
            FormPart::Field(Field {
                kind: "radio",
                options: Vec::new(),
                option_count: 0,
                group: Some(g.into()),
                unset: false,
                paging: false,
            })
        }),
    }
}

#[test]
fn generated_group_names_are_not_shown() {
    for name in [
        "0.43064796021664253",
        "mui-728401",
        "radio-f3a9c2e1b7",
        "123",
    ] {
        assert!(generated(name), "{name}");
    }
    for name in ["plan", "shipping_method", "q1", "answer[23]", "size-xl"] {
        assert!(!generated(name), "{name}");
    }
    let form = node(NodeKind::Structure, 0, "form", "", None);
    let set = node(NodeKind::Structure, 1, "radiogroup", "Period", None);
    let day = node(NodeKind::Actionable, 2, "radio", "1 day", Some("0.4306"));
    let week = node(NodeKind::Actionable, 2, "radio", "1 week", Some("0.4306"));
    let nodes = [form.clone(), set, day.clone(), week.clone()];
    let line = signature(&nodes[0], &nodes, &[2, 3], None);
    assert!(
        line.contains(r#"radio("Period"): "1 day""#),
        "the group's label: {line}"
    );
    // Without a labelled group around it, no name at all.
    let nodes = [form, day, week];
    let line = signature(&nodes[0], &nodes, &[1, 2], None);
    assert!(line.contains(r#"radio: "1 day""#), "{line}");
    assert!(!line.contains("0.4306"), "{line}");
}

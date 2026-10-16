//! What the server offers an agent to learn from, besides its tools: guides as MCP
//! resources (`aibrowse://guide/<name>`, the markdown in docs/guides/) and prompts,
//! task templates that set an agent on a good path through the tools ("compare the
//! price of X across shops" → find the shops, `survey` them, read the settings line).
//! The live session is a resource too (`aibrowse://session`), written by server.rs.

use std::collections::HashMap;

use rmcp::model::{Prompt, PromptArgument, PromptMessage, Resource, Role};

/// The guides, in reading order: (name, title, what it covers, the text).
const GUIDES: &[(&str, &str, &str, &str)] = &[
    (
        "overview",
        "How aibrowse works",
        "The tools and how they fit together; start here",
        include_str!("../../../docs/guides/overview.md"),
    ),
    (
        "goals",
        "Goals: letting the navigator click",
        "Writing goals, texts, parts (then) and keys; reading the report; checking a reached page",
        include_str!("../../../docs/guides/goals.md"),
    ),
    (
        "handoffs",
        "The user's part: handoffs and approvals",
        "Logins, 2FA, CAPTCHAs, approvals for committing actions, unattended sessions",
        include_str!("../../../docs/guides/handoffs.md"),
    ),
    (
        "research",
        "Research: search, survey, read",
        "Web searches judged by a criterion, one goal across many sites with filters, digests",
        include_str!("../../../docs/guides/research.md"),
    ),
    (
        "files",
        "Files: uploads and downloads",
        "Uploading local files (always approved) and where downloads go",
        include_str!("../../../docs/guides/files.md"),
    ),
    (
        "site-apis",
        "The site's own interfaces",
        "API calls learned while browsing, llms.txt, MCP, A2A and WebMCP front doors",
        include_str!("../../../docs/guides/site-apis.md"),
    ),
    (
        "policy",
        "Profiles and their policy",
        "Profiles, policy.json (approvals, agreements, recording, egress) and sessions",
        include_str!("../../../docs/guides/policy.md"),
    ),
];

/// The live session's resource.
pub const SESSION_URI: &str = "aibrowse://session";

const GUIDE_PREFIX: &str = "aibrowse://guide/";

/// Every resource: the guides, then the session.
pub fn resources() -> Vec<Resource> {
    let mut out: Vec<Resource> = GUIDES
        .iter()
        .map(|(name, title, about, _)| {
            Resource::new(format!("{GUIDE_PREFIX}{name}"), *name)
                .with_title(*title)
                .with_description(*about)
                .with_mime_type("text/markdown")
        })
        .collect();
    out.push(
        Resource::new(SESSION_URI, "session")
            .with_title("The current session")
            .with_description("Profile, settings (attended, approvals, agreements) and open tabs")
            .with_mime_type("text/markdown"),
    );
    out
}

/// A guide's text by its address, if it is one.
pub fn guide(uri: &str) -> Option<&'static str> {
    let name = uri.strip_prefix(GUIDE_PREFIX)?;
    GUIDES
        .iter()
        .find(|(n, ..)| *n == name)
        .map(|(.., text)| *text)
}

/// A prompt: its name, what it is for, its arguments (name, what, required), and how
/// its text is made from them.
struct Template {
    name: &'static str,
    about: &'static str,
    args: &'static [(&'static str, &'static str, bool)],
    text: fn(&Args) -> String,
}

/// A prompt's arguments as given.
struct Args<'a>(&'a HashMap<String, String>);

impl Args<'_> {
    fn get(&self, name: &str) -> &str {
        self.0.get(name).map_or("", |v| v.trim())
    }

    /// `text` with the argument, or `otherwise` when it wasn't given.
    fn or<'a>(&'a self, name: &str, otherwise: &'a str) -> &'a str {
        let value = self.get(name);
        if value.is_empty() { otherwise } else { value }
    }
}

const TEMPLATES: &[Template] = &[
    Template {
        name: "find_on_site",
        about: "Find a page or a fact on a website",
        args: &[
            (
                "site",
                "The site's address, e.g. https://www.example.com",
                true,
            ),
            ("what", "The page or fact to find, in plain words", true),
        ],
        text: |a| {
            format!(
                "Use aibrowse to find {what} on {site}.\n\n\
                 1. Call navigate(url=\"{site}\", goal=\"{what}\", digest=true). Pass texts=[...] with the search words the site would know it by, if you have better ones than the goal's.\n\
                 2. If it says Reached, check the page against the goal: the digest, and any \"It is set to:\" line. If something is off, act on it or call navigate again from there.\n\
                 3. If it's your turn, fill the named field (act fill) and call navigate with the goal again; if the user's, follow its advice (request_handoff).\n\
                 4. Answer with the fact and the page's address.\n\n\
                 The guide aibrowse://guide/goals explains goals and reports.",
                what = a.get("what"),
                site = a.get("site"),
            )
        },
    },
    Template {
        name: "compare_prices",
        about: "Compare a product's price and availability across several shops",
        args: &[
            (
                "product",
                "The product, as precisely as you know it (model number)",
                true,
            ),
            (
                "shops",
                "Shop addresses, comma-separated; found by search when left out",
                false,
            ),
            (
                "criteria",
                "Anything to narrow by: size, colour, new or used",
                false,
            ),
        ],
        text: |a| {
            let shops = a.get("shops");
            let find = if shops.is_empty() {
                "First find 4 to 8 shops that sell it where the user is: search(query=\"<product> price\", criterion=\"a shop's page selling it with its price\"), or shops you know.".to_owned()
            } else {
                format!("The shops: {shops}.")
            };
            format!(
                "Use aibrowse to compare {product} across shops.\n\n\
                 {find}\n\
                 Then call survey(sites=[...], goal=\"the product page for {product} with its price\", texts=[\"{product}\"], focus=\"the price, shipping, stock and any member or card discounts\"){filters}.\n\
                 Report a table: shop, price, availability, notes (discounts that need a card or membership). Say which sites didn't reach the product and why.\n\n\
                 The guide aibrowse://guide/research explains search and survey.",
                product = a.get("product"),
                filters = match a.get("criteria") {
                    "" => String::new(),
                    c => format!(", with filters=\"{c}\" if the goal is a listing"),
                },
            )
        },
    },
    Template {
        name: "research",
        about: "Answer a question from the web, with sources",
        args: &[
            ("question", "What to find out", true),
            (
                "criterion",
                "What a page must offer to count (a source, a kind of page)",
                false,
            ),
        ],
        text: |a| {
            format!(
                "Use aibrowse to answer: {question}\n\n\
                 1. search(query=<a good search query>, criterion=\"{criterion}\"). It opens the top results side by side and returns the pages that fulfil the criterion, with excerpts.\n\
                 2. For the best pages, read(view=\"digest\", focus=\"{question}\") after navigating there, or read in full when the excerpt isn't enough.\n\
                 3. Answer with the sources' addresses. Say where they disagree.",
                question = a.get("question"),
                criterion = a.or(
                    "criterion",
                    "a page that answers the question, from a reliable source"
                ),
            )
        },
    },
    Template {
        name: "fill_form",
        about: "Fill in a form on a site, stopping before anything is sent",
        args: &[
            (
                "site",
                "Where the form is: its address, or the site's",
                true,
            ),
            ("form", "Which form, in plain words", true),
            ("details", "What to fill in", true),
        ],
        text: |a| {
            format!(
                "Use aibrowse to fill in {form} on {site} with: {details}\n\n\
                 1. navigate(url=\"{site}\", goal=\"{form}\") to get to the form.\n\
                 2. observe, then act fill / select / set for each field from the details. The outline's form line lists the fields and their types.\n\
                 3. Don't submit unless the task says to: submitting is judged per action and waits for the user's approval when it commits something. Report what you filled in and what is left.\n\n\
                 The guide aibrowse://guide/handoffs explains approvals.",
                form = a.get("form"),
                site = a.get("site"),
                details = a.get("details"),
            )
        },
    },
    Template {
        name: "sign_in",
        about: "Get the user signed in to a site, through a handoff",
        args: &[
            ("site", "The site to sign in to", true),
            (
                "purpose",
                "What the sign-in is for, to tell the user",
                false,
            ),
        ],
        text: |a| {
            format!(
                "Use aibrowse to get the user signed in to {site}.\n\n\
                 1. navigate(url=\"{site}\") and look at the outline's auth: line. If signed in already, stop.\n\
                 2. Otherwise go to the sign-in page (navigate with goal=\"the sign-in page\") and call request_handoff(reason=\"Please sign in to {site} so I can {purpose}, then press Done\"). Never ask for the password in chat.\n\
                 3. Call handoff_status until it's done or cancelled. The login stays in this profile for later sessions.",
                site = a.get("site"),
                purpose = a.or("purpose", "continue"),
            )
        },
    },
    Template {
        name: "app_task",
        about: "Do a task in steps in a web app (a spreadsheet, a dashboard)",
        args: &[
            ("url", "The app's address", true),
            ("steps", "The steps, in order, one per line", true),
            (
                "texts",
                "Texts the steps type (cell addresses, formulas, names), comma-separated",
                false,
            ),
            (
                "keys",
                "Shortcuts the app uses, e.g. \"Meta+d: fill the selection down\"",
                false,
            ),
        ],
        text: |a| {
            let steps: Vec<&str> = a
                .get("steps")
                .lines()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect();
            let (first, rest) = steps.split_first().map_or(("", &[][..]), |(f, r)| (*f, r));
            format!(
                "Use aibrowse to do this in {url}:\n{list}\n\n\
                 Call navigate once with goal={first:?}, then={rest:?}, texts=[{texts}]{keys}. It does the steps in order and stops at the first it can't do, saying how to go on. Check the result on the page (observe, or read the fields it reports).\n\n\
                 The guide aibrowse://guide/goals explains parts, texts and keys.",
                url = a.get("url"),
                list = steps
                    .iter()
                    .map(|s| format!("- {s}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
                texts = a
                    .get("texts")
                    .split(',')
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                    .map(|t| format!("{t:?}"))
                    .collect::<Vec<_>>()
                    .join(", "),
                keys = match a.get("keys") {
                    "" => String::new(),
                    k => format!(", keys=[{k:?}]"),
                },
            )
        },
    },
    Template {
        name: "download_file",
        about: "Download a file from a site (a report, an invoice, a dataset)",
        args: &[
            ("site", "Where the file is", true),
            ("file", "Which file, in plain words", true),
        ],
        text: |a| {
            format!(
                "Use aibrowse to download {file} from {site}.\n\n\
                 1. navigate(url=\"{site}\", goal=\"{file}\"). The outline marks links that download: \"(downloads a file)\".\n\
                 2. Click it with act. The result says the download started or finished.\n\
                 3. downloads(wait_seconds=30) lists the session's downloads with their paths. Answer with the path.\n\n\
                 The guide aibrowse://guide/files explains uploads and downloads.",
                file = a.get("file"),
                site = a.get("site"),
            )
        },
    },
];

/// Every prompt.
pub fn prompts() -> Vec<Prompt> {
    TEMPLATES
        .iter()
        .map(|t| {
            let args = t
                .args
                .iter()
                .map(|(name, about, required)| {
                    PromptArgument::new(*name)
                        .with_description(*about)
                        .with_required(*required)
                })
                .collect();
            Prompt::new(t.name, Some(t.about), Some(args))
        })
        .collect()
}

/// The prompt `name` with `args`: its messages, or why it can't be made.
pub fn prompt(name: &str, args: &HashMap<String, String>) -> Result<Vec<PromptMessage>, String> {
    let template = TEMPLATES
        .iter()
        .find(|t| t.name == name)
        .ok_or_else(|| format!("no prompt {name:?}"))?;
    let missing: Vec<&str> = template
        .args
        .iter()
        .filter(|(arg, _, required)| {
            *required && args.get(*arg).is_none_or(|v| v.trim().is_empty())
        })
        .map(|(arg, ..)| *arg)
        .collect();
    if !missing.is_empty() {
        return Err(format!("prompt {name:?} needs: {}", missing.join(", ")));
    }
    Ok(vec![PromptMessage::new_text(
        Role::User,
        (template.text)(&Args(args)),
    )])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_guide_is_a_resource_and_readable() {
        let listed = resources();
        assert_eq!(listed.len(), GUIDES.len() + 1);
        for (name, ..) in GUIDES {
            let text = guide(&format!("aibrowse://guide/{name}")).unwrap();
            assert!(text.starts_with("# "), "{name}");
        }
        assert!(guide("aibrowse://guide/nothing").is_none());
    }

    #[test]
    fn guides_point_only_at_guides_that_exist() {
        for (name, .., text) in GUIDES {
            for (i, _) in text.match_indices("aibrowse://guide/") {
                let target: String = text[i + 17..]
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
                    .collect();
                assert!(
                    GUIDES.iter().any(|(n, ..)| *n == target),
                    "{name} points at {target}"
                );
            }
        }
    }

    #[test]
    fn prompts_need_their_required_arguments() {
        let mut args = HashMap::new();
        args.insert("site".to_owned(), "https://shop.example".to_owned());
        assert!(prompt("find_on_site", &args).unwrap_err().contains("what"));
        args.insert("what".to_owned(), "the return policy".to_owned());
        let messages = prompt("find_on_site", &args).unwrap();
        let text = serde_json::to_string(&messages).unwrap();
        assert!(text.contains("goal=\\\"the return policy\\\""), "{text}");
        assert!(prompt("nothing", &args).is_err());
    }

    #[test]
    fn an_app_task_is_one_navigate_in_parts() {
        let mut args = HashMap::new();
        args.insert("url".to_owned(), "https://sheet.example".to_owned());
        args.insert(
            "steps".to_owned(),
            "Enter 1 in A1\nSelect A1 to A10\nFill down".to_owned(),
        );
        args.insert("texts".to_owned(), "A1, 1, A1:A10".to_owned());
        let text = serde_json::to_string(&prompt("app_task", &args).unwrap()).unwrap();
        assert!(text.contains("goal=\\\"Enter 1 in A1\\\""), "{text}");
        assert!(
            text.contains("\\\"Select A1 to A10\\\", \\\"Fill down\\\""),
            "{text}"
        );
        assert!(text.contains("\\\"A1:A10\\\""), "{text}");
    }
}

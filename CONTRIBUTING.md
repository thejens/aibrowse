# Contributing

Thanks for helping. aibrowse is a Rust workspace (`crates/`); `make help` lists the
targets.

## Before you send a change

- `cargo fmt`, `cargo clippy --workspace --all-targets -- -D warnings`, and
  `cargo test --workspace` (unit and golden tests, no browser needed).
- Live tests drive a real Chrome: `make test-live` (`AIBROWSE_TEST_HEADLESS=1` to keep
  the window hidden). Tests that need the navigator skip themselves without a Jev key.
- Changes to the navigator, perception or reading are measured on the evals:
  `aibrowse eval navigator --suite evals/<suite>.json --profile eval-<suite>`. The bar
  is no false "reached" and the suites at or near their current scores. Say in the pull
  request what you ran.

## How the code is written

- Fix the general cause, not one site. A change made for one website should hold on
  others; say which sites showed the problem.
- Decisions about a page's meaning go through the navigator's model in any language:
  no word lists in one language for things like "next page" or "accept".
- Comments explain why: the constraint, the site that showed it, the trade-off.
- Files stay focused (about 500 lines at most); match the surrounding style.
- Commits use conventional prefixes (`feat:`, `fix:`, `docs:`, `refactor:`, `test:`,
  `chore:`).

## Licensing

Contributions are dual-licensed under MIT OR Apache-2.0, as the project is: unless you
say otherwise, any contribution you submit is licensed that way, without additional
terms.
